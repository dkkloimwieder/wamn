//! Pure Postgres text builders for project provisioning (SR3 house rule 3:
//! pure text + validated/quoted identifiers; the driver holds the connection).
//!
//! Every builder takes an **already-validated** project id (see
//! [`crate::validate_project_id`]); the database name it derives is
//! double-quoted, so a slug (which cannot contain a `"`) is injection-safe. The
//! `wamn_app` role name is a pinned constant. Values that vary (a probe's
//! database name, a role password) travel as `$n` params or quoted literals.

use crate::name::{APP_ROLE, database_name};
pub(crate) use wamn_pg_core::quote_ident;
use wamn_pg_core::quote_literal;
use wamn_run_state::{EFFECT_WRITER_ROLE, RUN_PROJECTION_WRITER_ROLE};

// Quote a SQL identifier (double-quoted, embedded `"` doubled). Mirrors the
// canonical `wamn_schema_compiler::sql::quote_ident` (inlined to keep this crate's
// dependency closure to `serde_json`).

// Quote a SQL string literal (single-quoted, embedded `'` doubled). Mirrors the
// canonical `wamn_schema_compiler::sql::quote_literal`.

/// Idempotently bootstrap the shared, cluster-global [`APP_ROLE`]. Runs in a
/// `DO` block so re-running against a cluster that already has the role is a
/// no-op. `NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS` — the role can only
/// do DML under RLS on tables explicitly granted to it (the S2/2.2 model). In
/// production the role is pre-created once; this makes the tool self-contained.
pub fn ensure_app_role_sql(password: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} LOGIN PASSWORD {pw} \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
           END IF; \
         END $$;",
        role = quote_ident(APP_ROLE),
        role_lit = quote_literal(APP_ROLE),
        pw = quote_literal(password),
    )
}

/// `CREATE DATABASE "<database>"`, naming the database directly (not derived from
/// a project id) — the per-project-env counterpart of [`create_database_sql`],
/// mirroring [`grant_connect_on_database_sql`] taking an already-derived name.
/// The name is double-quoted (a slug-derived name cannot contain a `"`, so it is
/// injection-safe). Must run as its own autocommit statement (Postgres forbids
/// `CREATE DATABASE` inside a transaction block).
///
/// Pass a name from [`project_env_database_name`](crate::project_env_database_name)
/// (`wamn-db-<org>--<project>--<env>`). In production the CNPG `Database` CRD
/// creates the per-project-env database; this is the plain-SQL equivalent the
/// substrate-agnostic gate uses off-cluster (wamn-q3n.8).
pub fn create_database_named_sql(database: &str) -> String {
    format!("CREATE DATABASE {}", quote_ident(database))
}

/// `CREATE DATABASE "wamn-db-<project>"`. Must run as its own autocommit
/// statement (Postgres forbids `CREATE DATABASE` inside a transaction block).
pub fn create_database_sql(project: &str) -> String {
    create_database_named_sql(&database_name(project))
}

/// `DROP DATABASE IF EXISTS "<database>" WITH (FORCE)`, naming the database
/// directly — the per-project-env counterpart of [`drop_database_sql`] (teardown /
/// gate only; destructive). Autocommit.
pub fn drop_database_named_sql(database: &str) -> String {
    format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        quote_ident(database)
    )
}

/// `DROP DATABASE IF EXISTS "wamn-db-<project>" WITH (FORCE)` — teardown / gate
/// only (destructive; the production tool never drops). Autocommit.
pub fn drop_database_sql(project: &str) -> String {
    drop_database_named_sql(&database_name(project))
}

/// Probe whether a database exists. The database **name** is the `$1`
/// parameter (a value, not an interpolated identifier); pass
/// [`database_name`]`(project)`.
pub fn database_exists_sql() -> &'static str {
    "SELECT EXISTS (SELECT FROM pg_database WHERE datname = $1)"
}

/// Restrict `CONNECT` on a database — named by its full, already-derived name —
/// to the shared app role: revoke `CONNECT` and `TEMPORARY` from `PUBLIC`
/// (PostgreSQL grants both on new databases by default) and grant `CONNECT` to
/// [`APP_ROLE`]. The name is
/// double-quoted (a slug-derived name cannot contain a `"`, so it is
/// injection-safe). Both statements are idempotent; issue as one batch.
///
/// This is the "thin imperative privilege step" the CNPG `Database` CRD does not
/// cover (per-project-env provisioning, wamn-q3n.7): the CRD creates the database
/// declaratively, but `REVOKE CONNECT FROM PUBLIC` / `GRANT` is run here. It is
/// defense-in-depth — the primary cross-project isolation is that a component is
/// routed to exactly one project's database (see the crate docs).
pub fn grant_connect_on_database_sql(database: &str) -> String {
    let db = quote_ident(database);
    format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE {db} FROM PUBLIC; \
         GRANT CONNECT ON DATABASE {db} TO {role};",
        role = quote_ident(APP_ROLE),
    )
}

/// Restrict `CONNECT` on the per-project database `wamn-db-<project>` (2.3) — a
/// thin wrapper over [`grant_connect_on_database_sql`] with the project's derived
/// database name.
pub fn grant_connect_sql(project: &str) -> String {
    grant_connect_on_database_sql(&database_name(project))
}

/// Idempotently create the stable effect-ledger ACL role as NOLOGIN.
///
/// Table/schema grants deliberately do not live here: schema-control owns them
/// once the effect-ledger tables exist. This builder only establishes the
/// cluster-global, ownership-free role identity and restrictive attributes.
pub fn ensure_effect_writer_acl_role_sql() -> String {
    format!(
        "DO $$ DECLARE role_name text; BEGIN \
           FOREACH role_name IN ARRAY ARRAY[{effect_lit}, {projection_lit}] LOOP \
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
               EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
             END IF; \
           END LOOP; \
         END $$;",
        effect_lit = quote_literal(EFFECT_WRITER_ROLE),
        projection_lit = quote_literal(RUN_PROJECTION_WRITER_ROLE),
    )
}

/// Prepare one inactive scoped credential generation for authenticated use.
///
/// `role` is the validated deterministic generation name. The caller verifies
/// the slot is inactive before applying this batch. Password and server-side
/// `VALID UNTIL` are replaced, membership is exactly the two stable ACL roles, and
/// direct database authority is solely `CONNECT` on this project database.
pub fn prepare_effect_writer_generation_sql(
    database: &str,
    role: &str,
    password: &str,
    expires_at: &str,
) -> String {
    let role_ident = quote_ident(role);
    let role_lit = quote_literal(role);
    format!(
        "{ensure} \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role_ident} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$; \
         ALTER ROLE {role_ident} LOGIN PASSWORD {password} VALID UNTIL {expires_at}; \
         GRANT {acl_role} TO {role_ident} \
           WITH ADMIN FALSE, INHERIT TRUE, SET TRUE; \
         GRANT {projection_role} TO {role_ident} \
           WITH ADMIN FALSE, INHERIT TRUE, SET TRUE; \
         GRANT CONNECT ON DATABASE {database} TO {role_ident};",
        ensure = ensure_effect_writer_acl_role_sql(),
        password = quote_literal(password),
        expires_at = quote_literal(expires_at),
        acl_role = quote_ident(EFFECT_WRITER_ROLE),
        projection_role = quote_ident(RUN_PROJECTION_WRITER_ROLE),
        database = quote_ident(database),
    )
}

/// Retire one old credential generation after replacement use was verified.
///
/// The batch removes authority and then authentication. The caller commits it
/// before terminating sessions with [`terminate_effect_writer_generation_sessions_sql`].
pub fn retire_effect_writer_generation_sql(database: &str, role: &str) -> String {
    let role_ident = quote_ident(role);
    format!(
        "REVOKE {acl_role}, {projection_role} FROM {role_ident}; \
         REVOKE CONNECT ON DATABASE {database} FROM {role_ident}; \
         ALTER ROLE {role_ident} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';",
        acl_role = quote_ident(EFFECT_WRITER_ROLE),
        projection_role = quote_ident(RUN_PROJECTION_WRITER_ROLE),
        database = quote_ident(database),
    )
}

/// Terminate sessions only after credential authority removal has committed.
pub fn terminate_effect_writer_generation_sessions_sql(role: &str) -> String {
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
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option AND m.set_option) \
                        FROM pg_auth_members m WHERE m.member = r.oid), true) \
              AS membership_options_exact, \
            COALESCE((SELECT array_agg(member.rolname::text ORDER BY member.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles member ON member.oid = m.member \
                       WHERE m.roleid = r.oid), ARRAY[]::text[]) AS member_roles, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option AND m.set_option) \
                        FROM pg_auth_members m WHERE m.roleid = r.oid), true) \
              AS member_options_exact, \
            NOT (EXISTS ( \
              SELECT 1 FROM pg_roles generation \
               WHERE generation.rolname ~ '^wamn_effect_writer_[0-9a-f]{40}_[ab]$' \
                 AND (has_database_privilege(generation.oid, current_database(), 'CONNECT') \
                      OR EXISTS ( \
                           SELECT 1 FROM pg_auth_members edge \
                           JOIN pg_roles parent ON parent.oid = edge.roleid \
                          WHERE edge.member = generation.oid \
                            AND parent.rolname IN ( \
                                  'wamn_effect_writer', 'wamn_run_projection_writer'))) \
                 AND (NOT generation.rolcanlogin OR generation.rolsuper \
                      OR generation.rolcreatedb OR generation.rolcreaterole \
                      OR NOT generation.rolinherit OR generation.rolreplication \
                      OR generation.rolbypassrls \
                      OR (SELECT array_agg(parent.rolname::text ORDER BY parent.rolname::text) \
                            FROM pg_auth_members edge \
                            JOIN pg_roles parent ON parent.oid = edge.roleid \
                           WHERE edge.member = generation.oid) \
                         IS DISTINCT FROM ARRAY[ \
                              'wamn_effect_writer', 'wamn_run_projection_writer']::text[] \
                      OR EXISTS (SELECT 1 FROM pg_auth_members edge \
                                  WHERE edge.member = generation.oid \
                                    AND (edge.admin_option OR NOT edge.inherit_option \
                                         OR NOT edge.set_option)) \
                      OR EXISTS (SELECT 1 FROM pg_auth_members edge \
                                  WHERE edge.roleid = generation.oid) \
                      OR EXISTS (SELECT 1 FROM pg_shdepend dependency \
                                  WHERE dependency.refclassid = 'pg_authid'::regclass \
                                    AND dependency.refobjid = generation.oid \
                                    AND dependency.deptype = 'o'))) \
              OR (SELECT count(*) FROM pg_roles generation \
                   WHERE generation.rolname ~ '^wamn_effect_writer_[0-9a-f]{40}_[ab]$' \
                     AND has_database_privilege( \
                           generation.oid, current_database(), 'CONNECT')) > 2 \
              OR (SELECT count(DISTINCT substring( \
                           generation.rolname FROM \
                           '^wamn_effect_writer_([0-9a-f]{40})_[ab]$')) \
                    FROM pg_roles generation \
                   WHERE generation.rolname ~ '^wamn_effect_writer_[0-9a-f]{40}_[ab]$' \
                     AND has_database_privilege( \
                           generation.oid, current_database(), 'CONNECT')) > 1) \
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

/// Read-only proof that no non-template database grants `CONNECT` to PUBLIC.
pub fn public_connect_databases_sql() -> &'static str {
    "SELECT d.datname::text FROM pg_database d \
      WHERE NOT d.datistemplate AND EXISTS ( \
        SELECT FROM aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl \
         WHERE acl.grantee = 0 AND acl.privilege_type = 'CONNECT') \
      ORDER BY d.datname::text"
}

/// Revoke PUBLIC `CONNECT` from every non-template database in this cluster.
///
/// Database privileges are cluster catalog entries, so the DO block may target
/// each database while connected to the exact project database. This gives ctl
/// ownership of converging the ratified cluster-wide floor during initial
/// generation preparation.
pub fn revoke_public_connect_floor_sql() -> &'static str {
    "DO $$ DECLARE database_name text; BEGIN \
       FOR database_name IN SELECT datname FROM pg_database WHERE NOT datistemplate LOOP \
         EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', database_name); \
       END LOOP; \
     END $$;"
}

/// Read-only proof of PUBLIC's effective `TEMPORARY` privilege on the target.
///
/// The caller connects to the already-validated exact project database. Unlike
/// the cluster-wide `CONNECT` floor, `TEMPORARY` is deliberately confined only
/// on that database so unrelated databases retain their own policy.
pub fn public_temporary_on_current_database_sql() -> &'static str {
    "SELECT EXISTS (SELECT FROM pg_database d CROSS JOIN LATERAL \
       aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl \
      WHERE d.datname = current_database() AND acl.grantee = 0 \
        AND acl.privilege_type = 'TEMPORARY')"
}

/// All non-template databases available for exact cross-database ACL proof.
pub fn non_template_databases_sql() -> &'static str {
    "SELECT datname::text FROM pg_database WHERE NOT datistemplate ORDER BY datname::text"
}

/// Read-only proof that PUBLIC has no `CONNECT` on any connectable database.
pub fn artifact_reader_public_connect_databases_sql() -> &'static str {
    "SELECT d.datname::text FROM pg_database d \
      WHERE d.datallowconn AND EXISTS ( \
        SELECT FROM aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl \
         WHERE acl.grantee=0 AND acl.privilege_type='CONNECT') \
      ORDER BY d.datname::text"
}

/// Converge the artifact reader's cluster-wide effective `CONNECT` floor.
pub fn artifact_reader_revoke_public_connect_floor_sql() -> &'static str {
    "DO $$ DECLARE database_name text; BEGIN \
       FOR database_name IN SELECT datname FROM pg_database WHERE datallowconn LOOP \
         EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', database_name); \
       END LOOP; \
     END $$;"
}

/// Remove the control database's ambient application-schema authority.
///
/// PostgreSQL grants PUBLIC `USAGE` on the `public` schema by default. The
/// artifact-reader contract permits only `catalog` schema `USAGE`, so the
/// first tenant-role installation converges that database-global floor in the
/// same transaction as the stable role and restrictive policy.
pub fn artifact_reader_revoke_public_schema_floor_sql() -> &'static str {
    "REVOKE ALL ON SCHEMA public FROM PUBLIC;"
}

/// Read-only direct ACL inventory for one role in the connected database.
pub fn role_database_acl_inventory_sql() -> &'static str {
    "WITH wanted AS (SELECT oid FROM pg_roles WHERE rolname = $1), acl AS ( \
       SELECT 'database'::text AS object_kind, d.datname::text AS schema_name, \
              d.datname::text AS object_name, x.privilege_type::text, x.is_grantable \
         FROM pg_database d CROSS JOIN LATERAL \
              aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) x \
        WHERE d.datname = current_database() AND x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'schema', n.nspname::text, n.nspname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_namespace n CROSS JOIN LATERAL \
              aclexplode(COALESCE(n.nspacl, acldefault('n', n.nspowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'relation', n.nspname::text, c.relname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl, acldefault('r', c.relowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'column', n.nspname::text, c.relname::text || '.' || a.attname::text, \
              x.privilege_type::text, x.is_grantable \
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         CROSS JOIN LATERAL aclexplode(a.attacl) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'type', n.nspname::text, t.typname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
         CROSS JOIN LATERAL aclexplode(COALESCE(t.typacl, acldefault('T', t.typowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'language', ''::text, l.lanname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_language l CROSS JOIN LATERAL \
              aclexplode(COALESCE(l.lanacl, acldefault('l', l.lanowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'large-object', ''::text, l.oid::text, x.privilege_type::text, x.is_grantable \
         FROM pg_largeobject_metadata l CROSS JOIN LATERAL \
              aclexplode(COALESCE(l.lomacl, acldefault('L', l.lomowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'foreign-data-wrapper', ''::text, f.fdwname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_foreign_data_wrapper f CROSS JOIN LATERAL \
              aclexplode(COALESCE(f.fdwacl, acldefault('F', f.fdwowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'foreign-server', ''::text, s.srvname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_foreign_server s CROSS JOIN LATERAL \
              aclexplode(COALESCE(s.srvacl, acldefault('S', s.srvowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'tablespace', ''::text, t.spcname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_tablespace t CROSS JOIN LATERAL \
              aclexplode(COALESCE(t.spcacl, acldefault('t', t.spcowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'parameter', ''::text, p.parname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_parameter_acl p CROSS JOIN LATERAL aclexplode(p.paracl) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'default-acl', COALESCE(n.nspname::text, ''), d.defaclobjtype::text, \
              x.privilege_type::text, x.is_grantable \
         FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid = d.defaclnamespace \
         CROSS JOIN LATERAL aclexplode(d.defaclacl) x \
        WHERE x.grantee = (SELECT oid FROM wanted)) \
     SELECT object_kind, schema_name, object_name, privilege_type, is_grantable FROM acl \
      ORDER BY object_kind, schema_name, object_name, privilege_type"
}

/// Inventory every effective artifact-reader authority in the control
/// application's schemas for one exact role.
///
/// `$1` is an existing stable or generation role. Unlike the direct ACL
/// inventory, the `has_*_privilege` predicates include privileges inherited
/// from PUBLIC and role membership. System schemas are deliberately excluded;
/// the credential contract governs only the control application's `catalog`
/// and `wamn_run` surfaces.
pub fn artifact_reader_effective_authority_sql() -> &'static str {
    "WITH wanted AS (SELECT oid FROM pg_roles WHERE rolname=$1), \
          app_schemas AS (SELECT oid, nspname FROM pg_namespace \
                           WHERE nspname <> 'information_schema' \
                             AND nspname !~ '^pg_'), \
          authority AS ( \
       SELECT 'database'::text AS object_kind, ''::text AS schema_name, \
              d.datname::text AS object_name, 'CONNECT'::text AS privilege \
         FROM wanted w JOIN pg_database d ON d.datallowconn \
        WHERE has_database_privilege(w.oid, d.oid, 'CONNECT') \
       UNION ALL \
       SELECT 'database', '', d.datname::text, privilege::text \
         FROM wanted w JOIN pg_database d ON d.datname=current_database() \
         CROSS JOIN (VALUES ('CREATE'),('TEMPORARY')) p(privilege) \
        WHERE has_database_privilege(w.oid, d.oid, privilege) \
       UNION ALL \
       SELECT 'schema'::text AS object_kind, n.nspname::text AS schema_name, \
              n.nspname::text AS object_name, privilege::text \
         FROM wanted w CROSS JOIN app_schemas n \
         CROSS JOIN (VALUES ('USAGE'),('CREATE')) p(privilege) \
        WHERE has_schema_privilege(w.oid, n.oid, privilege) \
       UNION ALL \
       SELECT 'relation', n.nspname::text, c.relname::text, privilege::text \
         FROM wanted w JOIN pg_class c ON c.relkind IN ('r','p','v','m','f') \
         JOIN app_schemas n ON n.oid=c.relnamespace \
         CROSS JOIN (VALUES ('SELECT'),('INSERT'),('UPDATE'),('DELETE'), \
                            ('TRUNCATE'),('REFERENCES'),('TRIGGER'),('MAINTAIN')) p(privilege) \
        WHERE has_table_privilege(w.oid, c.oid, privilege) \
       UNION ALL \
       SELECT 'column', n.nspname::text, \
              c.relname::text || '.' || a.attname::text, privilege::text \
         FROM wanted w JOIN pg_attribute a ON a.attnum > 0 AND NOT a.attisdropped \
         JOIN pg_class c ON c.oid=a.attrelid AND c.relkind IN ('r','p','v','m','f') \
         JOIN app_schemas n ON n.oid=c.relnamespace \
         CROSS JOIN (VALUES ('SELECT'),('INSERT'),('UPDATE'),('REFERENCES')) p(privilege) \
        WHERE has_column_privilege(w.oid, c.oid, a.attnum, privilege) \
       UNION ALL \
       SELECT 'sequence', n.nspname::text, c.relname::text, privilege::text \
         FROM wanted w JOIN pg_class c ON c.relkind='S' \
         JOIN app_schemas n ON n.oid=c.relnamespace \
         CROSS JOIN (VALUES ('USAGE'),('SELECT'),('UPDATE')) p(privilege) \
        WHERE has_sequence_privilege(w.oid, c.oid, privilege) \
       UNION ALL \
       SELECT 'routine', n.nspname::text, \
              p.proname::text || '(' || pg_get_function_identity_arguments(p.oid) || ')', \
              'EXECUTE'::text \
         FROM wanted w JOIN pg_proc p ON has_function_privilege(w.oid, p.oid, 'EXECUTE') \
         JOIN app_schemas n ON n.oid=p.pronamespace \
        WHERE p.prosecdef OR NOT EXISTS (SELECT 1 FROM pg_depend d \
              WHERE d.classid='pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e')) \
     SELECT object_kind, schema_name, object_name, privilege FROM authority \
      ORDER BY object_kind, schema_name, object_name, privilege"
}

/// Inventory direct/default PUBLIC authority in control application schemas.
///
/// This preflight is valid before the deterministic reader roles exist. A
/// nonempty result means a future exact generation would inherit application
/// authority outside its tenant role even if its own ACLs were pristine.
pub fn artifact_reader_public_authority_sql() -> &'static str {
    "WITH app_schemas AS (SELECT oid, nspname, nspowner, nspacl FROM pg_namespace \
                           WHERE nspname <> 'information_schema' \
                             AND nspname !~ '^pg_'), \
          app_owners AS (SELECT nspowner AS oid FROM app_schemas \
                         UNION SELECT c.relowner FROM pg_class c \
                           JOIN app_schemas n ON n.oid=c.relnamespace \
                         UNION SELECT p.proowner FROM pg_proc p \
                           JOIN app_schemas n ON n.oid=p.pronamespace), \
          authority AS ( \
       SELECT 'schema'::text AS object_kind, n.nspname::text AS schema_name, \
              n.nspname::text AS object_name, x.privilege_type::text \
         FROM app_schemas n CROSS JOIN LATERAL aclexplode(n.nspacl) x \
        WHERE x.grantee=0 \
       UNION ALL \
       SELECT CASE WHEN c.relkind='S' THEN 'sequence' ELSE 'relation' END, \
              n.nspname::text, c.relname::text, x.privilege_type::text \
         FROM pg_class c JOIN app_schemas n ON n.oid=c.relnamespace \
         CROSS JOIN LATERAL aclexplode(c.relacl) x \
        WHERE c.relkind IN ('r','p','v','m','f','S') AND x.grantee=0 \
       UNION ALL \
       SELECT 'column', n.nspname::text, \
              c.relname::text || '.' || a.attname::text, x.privilege_type::text \
         FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid \
         JOIN app_schemas n ON n.oid=c.relnamespace \
         CROSS JOIN LATERAL aclexplode(a.attacl) x \
        WHERE a.attnum > 0 AND NOT a.attisdropped AND x.grantee=0 \
       UNION ALL \
       SELECT 'routine', n.nspname::text, \
              p.proname::text || '(' || pg_get_function_identity_arguments(p.oid) || ')', \
              x.privilege_type::text \
         FROM pg_proc p JOIN app_schemas n ON n.oid=p.pronamespace \
         CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) x \
        WHERE x.grantee=0 AND (p.prosecdef OR NOT EXISTS (SELECT 1 FROM pg_depend d \
              WHERE d.classid='pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e')) \
       UNION ALL \
       SELECT 'type', n.nspname::text, t.typname::text, x.privilege_type::text \
         FROM pg_type t JOIN app_schemas n ON n.oid=t.typnamespace \
         CROSS JOIN LATERAL \
              aclexplode(COALESCE(t.typacl, acldefault('T', t.typowner))) x \
        WHERE x.grantee=0 \
          AND (x.privilege_type <> 'USAGE' OR x.is_grantable) \
       UNION ALL \
       SELECT 'language', ''::text, l.lanname::text, x.privilege_type::text \
         FROM pg_language l CROSS JOIN LATERAL \
              aclexplode(COALESCE(l.lanacl, acldefault('l', l.lanowner))) x \
        WHERE x.grantee=0 \
          AND (x.privilege_type <> 'USAGE' OR x.is_grantable) \
       UNION ALL \
       SELECT 'large-object', ''::text, l.oid::text, x.privilege_type::text \
         FROM pg_largeobject_metadata l CROSS JOIN LATERAL \
              aclexplode(COALESCE(l.lomacl, acldefault('L', l.lomowner))) x \
        WHERE x.grantee=0 \
       UNION ALL \
       SELECT 'foreign-data-wrapper', ''::text, f.fdwname::text, x.privilege_type::text \
         FROM pg_foreign_data_wrapper f CROSS JOIN LATERAL \
              aclexplode(COALESCE(f.fdwacl, acldefault('F', f.fdwowner))) x \
        WHERE x.grantee=0 \
       UNION ALL \
       SELECT 'foreign-server', ''::text, s.srvname::text, x.privilege_type::text \
         FROM pg_foreign_server s CROSS JOIN LATERAL \
              aclexplode(COALESCE(s.srvacl, acldefault('S', s.srvowner))) x \
        WHERE x.grantee=0 \
       UNION ALL \
       SELECT 'tablespace', ''::text, t.spcname::text, x.privilege_type::text \
         FROM pg_tablespace t CROSS JOIN LATERAL \
              aclexplode(COALESCE(t.spcacl, acldefault('t', t.spcowner))) x \
        WHERE x.grantee=0 \
       UNION ALL \
       SELECT 'parameter', ''::text, p.parname::text, x.privilege_type::text \
         FROM pg_parameter_acl p CROSS JOIN LATERAL aclexplode(p.paracl) x \
        WHERE x.grantee=0 \
       UNION ALL \
       SELECT 'default-acl', COALESCE(n.nspname::text, ''), d.defaclobjtype::text, \
              x.privilege_type::text \
         FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid=d.defaclnamespace \
         CROSS JOIN LATERAL aclexplode(d.defaclacl) x \
        WHERE d.defaclrole IN (SELECT oid FROM app_owners) \
          AND (d.defaclnamespace=0 OR d.defaclnamespace IN (SELECT oid FROM app_schemas)) \
          AND x.grantee=0) \
     SELECT object_kind, schema_name, object_name, privilege_type FROM authority \
      ORDER BY object_kind, schema_name, object_name, privilege_type"
}

/// Install one stable, tenant-scoped control artifact-reader ACL role.
///
/// `role` and `policy` are validated deterministic names. `marker` is the
/// exact non-secret JSON owner marker. The ordinary `execution_bundles_tenant`
/// policy remains the `app.tenant` consistency check; this role-targeted,
/// tenant-literal restrictive policy is the structural cross-tenant boundary.
/// The caller probes existing state and refuses drift before applying this
/// idempotent transaction fragment.
pub fn install_artifact_reader_tenant_role_sql(
    role: &str,
    policy: &str,
    tenant_id: &str,
    marker: &str,
) -> String {
    let role_ident = quote_ident(role);
    let role_lit = quote_literal(role);
    let policy_ident = quote_ident(policy);
    let policy_lit = quote_literal(policy);
    format!(
        "{} \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role_ident} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$; \
         COMMENT ON ROLE {role_ident} IS {marker}; \
         GRANT USAGE ON SCHEMA catalog TO {role_ident}; \
         GRANT SELECT (tenant_id, execution_bundle_hash, format_version, \
           exact_bytes, byte_length) ON TABLE catalog.execution_bundles TO {role_ident}; \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_policy \
             WHERE polrelid = 'catalog.execution_bundles'::regclass \
               AND polname = {policy_lit}) THEN \
             CREATE POLICY {policy_ident} ON catalog.execution_bundles \
               AS RESTRICTIVE FOR SELECT TO {role_ident} \
               USING (tenant_id = {tenant}); \
           END IF; \
         END $$;",
        artifact_reader_revoke_public_schema_floor_sql(),
        marker = quote_literal(marker),
        tenant = quote_literal(tenant_id),
    )
}

/// Prepare one inactive artifact-reader A/B generation for authenticated use.
///
/// The caller first proves that an existing slot is completely inactive. The
/// generation inherits exactly one stable ACL role but cannot `SET ROLE` to it;
/// its `app.tenant` setting is scoped to the exact control database.
pub fn prepare_artifact_reader_generation_sql(
    database: &str,
    stable_role: &str,
    generation_role: &str,
    tenant_id: &str,
    password: &str,
    expires_at: &str,
    marker: &str,
) -> String {
    let database_ident = quote_ident(database);
    let stable_ident = quote_ident(stable_role);
    let generation_ident = quote_ident(generation_role);
    let generation_lit = quote_literal(generation_role);
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {generation_lit}) THEN \
             CREATE ROLE {generation_ident} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$; \
         COMMENT ON ROLE {generation_ident} IS {marker}; \
         ALTER ROLE {generation_ident} LOGIN PASSWORD {password} VALID UNTIL {expires_at}; \
         GRANT {stable_ident} TO {generation_ident} \
           WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; \
         ALTER ROLE {generation_ident} IN DATABASE {database_ident} \
           SET app.tenant TO {tenant}; \
         REVOKE TEMPORARY ON DATABASE {database_ident} FROM PUBLIC; \
         GRANT CONNECT ON DATABASE {database_ident} TO {generation_ident};",
        marker = quote_literal(marker),
        password = quote_literal(password),
        expires_at = quote_literal(expires_at),
        tenant = quote_literal(tenant_id),
    )
}

/// Remove an old artifact-reader generation's authority before terminating it.
///
/// The role marker intentionally remains: inactive-slot verification still
/// needs the exact non-secret scope identity before a later A/B reuse.
pub fn retire_artifact_reader_generation_sql(
    database: &str,
    stable_role: &str,
    generation_role: &str,
) -> String {
    let database = quote_ident(database);
    let stable_role = quote_ident(stable_role);
    let generation_role = quote_ident(generation_role);
    format!(
        "REVOKE {stable_role} FROM {generation_role}; \
         REVOKE CONNECT ON DATABASE {database} FROM {generation_role}; \
         ALTER ROLE {generation_role} IN DATABASE {database} RESET app.tenant; \
         ALTER ROLE {generation_role} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';"
    )
}

/// Terminate sessions only after artifact-reader authority removal commits.
pub fn terminate_artifact_reader_generation_sessions_sql(role: &str) -> String {
    format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
           WHERE usename = {} AND pid <> pg_backend_pid();",
        quote_literal(role)
    )
}

/// Read-only exact state for a stable artifact-reader tenant ACL role.
///
/// `$1` is the stable role name. Direct ACLs and the RLS policy are deliberately
/// separate inventories so a caller must compare every authority family.
pub fn artifact_reader_tenant_role_state_sql() -> &'static str {
    "SELECT r.rolcanlogin, r.rolsuper, r.rolinherit, r.rolcreaterole, \
            r.rolcreatedb, r.rolreplication, r.rolbypassrls, \
            r.rolpassword IS NOT NULL AS password_set, \
            CASE WHEN r.rolvaliduntil IS NULL THEN NULL \
                 ELSE to_char(r.rolvaliduntil AT TIME ZONE 'UTC', \
                              'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') END AS valid_until, \
            COALESCE(isfinite(r.rolvaliduntil), false) AS valid_until_finite, \
            pg_catalog.shobj_description(r.oid, 'pg_authid') AS marker, \
            COALESCE((SELECT array_agg(parent.rolname::text ORDER BY parent.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles parent ON parent.oid=m.roleid \
                       WHERE m.member=r.oid), ARRAY[]::text[]) AS memberships, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option \
                                      AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.member=r.oid), true) \
              AS membership_options_exact, \
            COALESCE((SELECT array_agg(member.rolname::text ORDER BY member.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles member ON member.oid=m.member \
                       WHERE m.roleid=r.oid), ARRAY[]::text[]) AS member_roles, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option \
                                      AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.roleid=r.oid), true) \
              AS member_options_exact, \
            COALESCE((SELECT array_agg(COALESCE(d.datname::text, '*') || ':' || setting \
                                      ORDER BY COALESCE(d.datname::text, '*'), setting) \
                        FROM pg_db_role_setting s LEFT JOIN pg_database d ON d.oid=s.setdatabase \
                        CROSS JOIN LATERAL unnest(s.setconfig) setting \
                       WHERE s.setrole=r.oid), ARRAY[]::text[]) AS database_settings, \
            COALESCE((SELECT array_agg(d.datname::text ORDER BY d.datname::text) \
                        FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) acl \
                       WHERE acl.grantee=r.oid AND acl.privilege_type='CONNECT'), \
                     ARRAY[]::text[]) AS connect_databases, \
            COALESCE((SELECT array_agg(d.datname::text ORDER BY d.datname::text) \
                        FROM pg_database d WHERE d.datallowconn \
                          AND has_database_privilege(r.oid, d.oid, 'CONNECT')), \
                     ARRAY[]::text[]) AS effective_connect_databases, \
            (SELECT count(*)::bigint FROM pg_stat_activity a \
              WHERE a.usename=r.rolname) AS sessions, \
            (SELECT count(*)::bigint FROM pg_shdepend d \
              WHERE d.refclassid='pg_authid'::regclass AND d.refobjid=r.oid \
                AND d.deptype='o') AS owned_objects \
       FROM pg_catalog.pg_authid r WHERE r.rolname=$1"
}

/// Read-only exact state for one artifact-reader credential generation.
///
/// `$1` is the generation role. Database-scoped settings are returned with the
/// database name so a role-global or foreign-database `app.tenant` cannot hide.
pub fn artifact_reader_generation_state_sql() -> &'static str {
    "SELECT r.rolcanlogin, r.rolsuper, r.rolinherit, r.rolcreaterole, \
            r.rolcreatedb, r.rolreplication, r.rolbypassrls, \
            r.rolpassword IS NOT NULL AS password_set, \
            CASE WHEN r.rolvaliduntil IS NULL THEN NULL \
                 ELSE to_char(r.rolvaliduntil AT TIME ZONE 'UTC', \
                              'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') END AS valid_until, \
            COALESCE(isfinite(r.rolvaliduntil), false) AS valid_until_finite, \
            pg_catalog.shobj_description(r.oid, 'pg_authid') AS marker, \
            COALESCE((SELECT array_agg(parent.rolname::text ORDER BY parent.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles parent ON parent.oid=m.roleid \
                       WHERE m.member=r.oid), ARRAY[]::text[]) AS memberships, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option \
                                      AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.member=r.oid), true) \
              AS membership_options_exact, \
            COALESCE((SELECT array_agg(member.rolname::text ORDER BY member.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles member ON member.oid=m.member \
                       WHERE m.roleid=r.oid), ARRAY[]::text[]) AS member_roles, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option \
                                      AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.roleid=r.oid), true) \
              AS member_options_exact, \
            COALESCE((SELECT array_agg(COALESCE(d.datname::text, '*') || ':' || setting \
                                      ORDER BY COALESCE(d.datname::text, '*'), setting) \
                        FROM pg_db_role_setting s LEFT JOIN pg_database d ON d.oid=s.setdatabase \
                        CROSS JOIN LATERAL unnest(s.setconfig) setting \
                       WHERE s.setrole=r.oid), ARRAY[]::text[]) AS database_settings, \
            COALESCE((SELECT array_agg(d.datname::text ORDER BY d.datname::text) \
                        FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) acl \
                       WHERE acl.grantee=r.oid AND acl.privilege_type='CONNECT'), \
                     ARRAY[]::text[]) AS connect_databases, \
            COALESCE((SELECT array_agg(d.datname::text ORDER BY d.datname::text) \
                        FROM pg_database d WHERE d.datallowconn \
                          AND has_database_privilege(r.oid, d.oid, 'CONNECT')), \
                     ARRAY[]::text[]) AS effective_connect_databases, \
            (SELECT count(*)::bigint FROM pg_stat_activity a \
              WHERE a.usename=r.rolname) AS sessions, \
            (SELECT count(*)::bigint FROM pg_shdepend d \
              WHERE d.refclassid='pg_authid'::regclass AND d.refobjid=r.oid \
                AND d.deptype='o') AS owned_objects \
       FROM pg_catalog.pg_authid r WHERE r.rolname=$1"
}

/// Read the RLS enforcement flags for the protected bundle relation.
pub fn artifact_reader_rls_state_sql() -> &'static str {
    "SELECT c.relrowsecurity, c.relforcerowsecurity FROM pg_class c \
      WHERE c.oid='catalog.execution_bundles'::regclass"
}

/// Read every execution-bundle policy applicable to PUBLIC, the stable role,
/// or the exact generation role.
///
/// `$1` is the stable role, `$2` is a generation role (or the empty string),
/// and `$3` is the deterministic tenant-policy name. The name predicate makes
/// a pre-existing collision visible even before either expected role exists.
/// Policies for other tenant roles remain irrelevant because generations have
/// exactly one parent; PUBLIC and direct generation policies can never hide.
pub fn artifact_reader_policy_state_sql() -> &'static str {
    "SELECT p.polname, p.polcmd::text, p.polpermissive, \
            ARRAY(SELECT CASE role_oid WHEN 0 THEN 'PUBLIC' \
                              ELSE pg_get_userbyid(role_oid) END \
                    FROM unnest(p.polroles) role_oid ORDER BY 1), \
            pg_get_expr(p.polqual,p.polrelid,false), \
            pg_get_expr(p.polwithcheck,p.polrelid,false) \
      FROM pg_catalog.pg_policy p \
      WHERE p.polrelid='catalog.execution_bundles'::regclass \
        AND (0::oid=ANY(p.polroles) \
             OR (SELECT oid FROM pg_roles WHERE rolname=$1)=ANY(p.polroles) \
             OR (SELECT oid FROM pg_roles WHERE rolname=$2)=ANY(p.polroles) \
             OR p.polname=$3) \
      ORDER BY p.polname"
}

/// Read the non-secret session receipt for one artifact-reader generation.
///
/// `$1` is the generation role. Query text and client addresses are excluded.
pub fn artifact_reader_generation_sessions_sql() -> &'static str {
    "SELECT pid, datname, application_name, state \
       FROM pg_catalog.pg_stat_activity WHERE usename=$1 ORDER BY pid"
}

/// Count exact replacement-use sessions before retiring the old generation.
///
/// `$1` is generation role, `$2` control database, and `$3` the frozen executor
/// artifact-reader application name.
pub fn artifact_reader_replacement_use_sql() -> &'static str {
    "SELECT count(*)::bigint FROM pg_catalog.pg_stat_activity \
      WHERE usename=$1 AND datname=$2 AND application_name=$3"
}

/// Acquire the session-scoped serialization lock for one reader scope.
pub fn artifact_reader_scope_lock_sql() -> &'static str {
    "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
}

/// Release the exact session-scoped serialization lock for one reader scope.
pub fn artifact_reader_scope_unlock_sql() -> &'static str {
    "SELECT pg_advisory_unlock(hashtextextended($1::text, 0))"
}

/// Session-scoped serialization key for one project-environment rotation.
pub fn effect_writer_scope_lock_sql() -> &'static str {
    "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
}

// --- CDC capture provisioning (wamn-l5i9.9, D19 v3 §4) -----------------------
//
// The per-project-env CDC substrate: a REPLICATION role (the R8b credential
// tier above `wamn_app` query creds and the dispatch role), a publication over
// the app data schema, and a failover-enabled logical replication slot. The
// publication and the slot are DATABASE-BOUND — apply their SQL connected to
// the project-env database; the role is cluster-global. Pass the shared
// `cdc_object_name` (`wamn_cdc_<org>__<project>__<env>`) as the role /
// publication / slot name.

/// Idempotently bootstrap a per-project-env **replication** role: `REPLICATION
/// LOGIN`, otherwise least-privilege (`NOSUPERUSER NOCREATEDB NOCREATEROLE
/// NOBYPASSRLS`). One role per project-env (a leaked credential's blast radius
/// is one registration) — but note `REPLICATION` itself is CLUSTER-WIDE in
/// Postgres: any replication role can read any database's WAL on that cluster,
/// so on a shared pool the input-side isolation rests on handing each reader
/// only its own slot/publication/credentials (documented in the runbook).
pub fn ensure_replication_role_sql(role: &str, password: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} LOGIN REPLICATION PASSWORD {pw} \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
           END IF; \
         END $$;",
        role = quote_ident(role),
        role_lit = quote_literal(role),
        pw = quote_literal(password),
    )
}

/// `CREATE SCHEMA IF NOT EXISTS "<schema>"` — the eager guard that makes the
/// CDC SQL order-robust: `FOR TABLES IN SCHEMA` auto-includes tables created
/// later, so the publication may be created BEFORE catalog-publish fills the
/// schema and still capture everything from the start. Catalog-publish's own
/// `CREATE SCHEMA IF NOT EXISTS` then no-ops.
pub fn ensure_schema_sql(schema: &str) -> String {
    format!("CREATE SCHEMA IF NOT EXISTS {}", quote_ident(schema))
}

/// Idempotently create the CDC publication over the project-env's app **data**
/// schema: `CREATE PUBLICATION <pub> FOR TABLES IN SCHEMA <schema>`, guarded by
/// a `pg_publication` probe (Postgres has no `CREATE PUBLICATION IF NOT
/// EXISTS`). `FOR TABLES IN SCHEMA` auto-includes tables created in the schema
/// later — the D19 v3 replacement for the retired per-table trigger emission.
/// Re-pointing an existing publication at a different schema is a manual
/// `ALTER PUBLICATION … SET TABLES IN SCHEMA` (the guard never rewrites).
/// Run connected to the project-env database.
pub fn create_publication_sql(publication: &str, schema: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_publication WHERE pubname = {pub_lit}) THEN \
             CREATE PUBLICATION {publication} FOR TABLES IN SCHEMA {schema}; \
           END IF; \
         END $$;",
        publication = quote_ident(publication),
        pub_lit = quote_literal(publication),
        schema = quote_ident(schema),
    )
}

/// Idempotently create the **failover-enabled** logical replication slot via
/// the SQL-function form: `pg_create_logical_replication_slot(<slot>,
/// 'pgoutput', temporary => false, twophase => false, failover => true)`
/// (PG17+ fifth argument) — a normal connection, no replication-protocol
/// syntax; the reader's `ensure_replication_slot` tolerates the existing slot
/// (same plugin/twophase/failover shape). Logical slots are DATABASE-BOUND:
/// run connected to the project-env database. WAL is pinned from creation
/// (capture starts at CDC-enable), bounded by the cluster's
/// `max_slot_wal_keep_size`.
pub fn create_failover_slot_sql(slot: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {slot_lit}) THEN \
             PERFORM pg_create_logical_replication_slot({slot_lit}, 'pgoutput', false, false, true); \
           END IF; \
         END $$;",
        slot_lit = quote_literal(slot),
    )
}

/// Grant the replication role its read surface: `CONNECT` on the project-env
/// database, `USAGE` on the app data schema, and `SELECT` on the schema's
/// **current** tables (an initial-snapshot/backfill needs table reads; logical
/// *decoding* itself reads WAL, not tables, so tables added later decode fine
/// without a re-grant). Idempotent; run connected to the project-env database
/// AFTER the schema exists.
pub fn grant_replication_access_sql(database: &str, role: &str, schema: &str) -> String {
    let role = quote_ident(role);
    format!(
        "GRANT CONNECT ON DATABASE {db} TO {role}; \
         GRANT USAGE ON SCHEMA {schema} TO {role}; \
         GRANT SELECT ON ALL TABLES IN SCHEMA {schema} TO {role};",
        db = quote_ident(database),
        schema = quote_ident(schema),
    )
}

/// The decode-time entity map (wamn-l5i9.11, D19 v3 §4): `relation_oid` →
/// stable catalog entity id. **OID-keyed** so a reader resolving events is
/// timeless under catch-up — pg_class OIDs survive `ALTER TABLE RENAME`, so a
/// session decoding pre-rename backlog still resolves correctly, and a rename
/// only updates the informational `table_name`. Maintained by
/// `publish-catalog`/`migrate-catalog` in the same transaction as the DDL;
/// rows are upsert-only (a dropped entity's row keeps old-WAL decode
/// resolvable). No RLS: it holds no tenant data, and the CDC role's decode
/// stream sees every row of every table anyway.
pub fn ensure_entity_map_sql(schema: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {schema}.wamn_entities ( \
           relation_oid oid PRIMARY KEY, \
           entity_id text NOT NULL, \
           table_name text NOT NULL)",
        schema = quote_ident(schema),
    )
}

/// Upsert one entity's map row, resolving the table's CURRENT `pg_class` OID
/// server-side — run in the SAME transaction as the DDL that created/renamed
/// the table, so the row is atomic with the physical state. `$1` = entity id,
/// `$2` = physical table name. A table that does not exist (a catalog entity
/// whose floor was never applied) upserts nothing — the SELECT is empty.
pub fn upsert_entity_map_sql(schema: &str) -> String {
    format!(
        "INSERT INTO {schema}.wamn_entities (relation_oid, entity_id, table_name) \
         SELECT c.oid, $1, $2::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema_lit} AND c.relname = $2::text AND c.relkind = 'r' \
         ON CONFLICT (relation_oid) DO UPDATE \
           SET entity_id = EXCLUDED.entity_id, table_name = EXCLUDED.table_name",
        schema = quote_ident(schema),
        schema_lit = quote_literal(schema),
    )
}

/// `DROP PUBLICATION IF EXISTS "<publication>"` — teardown / gate only.
pub fn drop_publication_sql(publication: &str) -> String {
    format!("DROP PUBLICATION IF EXISTS {}", quote_ident(publication))
}

/// Drop the replication slot if it exists (teardown / gate only — dropping a
/// live slot severs the reader and releases the pinned WAL). Run connected to
/// the slot's database.
pub fn drop_replication_slot_sql(slot: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {slot_lit}) THEN \
             PERFORM pg_drop_replication_slot({slot_lit}); \
           END IF; \
         END $$;",
        slot_lit = quote_literal(slot),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_app_role_is_least_privilege_and_idempotent() {
        let sql = ensure_app_role_sql("wamn_app");
        assert!(sql.contains("IF NOT EXISTS"), "idempotent guard");
        assert!(sql.contains("CREATE ROLE \"wamn_app\""));
        assert!(sql.contains("PASSWORD 'wamn_app'"));
        // Least privilege — every restrictive attribute is present.
        for attr in ["NOSUPERUSER", "NOCREATEDB", "NOCREATEROLE", "NOBYPASSRLS"] {
            assert!(sql.contains(attr), "missing {attr}");
        }
        // A password with a quote is escaped, not injected.
        assert!(ensure_app_role_sql("a'b").contains("PASSWORD 'a''b'"));
    }

    #[test]
    fn database_ddl_quotes_the_hyphenated_name() {
        assert_eq!(
            create_database_sql("acme-corp"),
            "CREATE DATABASE \"wamn-db-acme-corp\""
        );
        assert_eq!(
            drop_database_sql("acme-corp"),
            "DROP DATABASE IF EXISTS \"wamn-db-acme-corp\" WITH (FORCE)"
        );
    }

    #[test]
    fn named_database_ddl_targets_an_arbitrary_db_name_and_the_wrappers_delegate() {
        // The per-project-env path (wamn-q3n.7/.8) passes a full triple-derived name.
        assert_eq!(
            create_database_named_sql("wamn-db-acme--billing--dev"),
            "CREATE DATABASE \"wamn-db-acme--billing--dev\""
        );
        assert_eq!(
            drop_database_named_sql("wamn-db-acme--billing--dev"),
            "DROP DATABASE IF EXISTS \"wamn-db-acme--billing--dev\" WITH (FORCE)"
        );
        // The 2.3 project-taking wrappers delegate to the named builders.
        assert_eq!(
            create_database_sql("acme"),
            create_database_named_sql("wamn-db-acme")
        );
        assert_eq!(
            drop_database_sql("acme"),
            drop_database_named_sql("wamn-db-acme")
        );
    }

    #[test]
    fn grant_connect_revokes_public_then_grants_app_role() {
        let sql = grant_connect_sql("acme");
        // The REVOKE FROM PUBLIC must precede the GRANT (order is not load-bearing
        // for correctness, but both must be present — the isolation backstop).
        let revoke = sql
            .find("REVOKE CONNECT, TEMPORARY ON DATABASE \"wamn-db-acme\" FROM PUBLIC")
            .expect("revoke public");
        let grant = sql
            .find("GRANT CONNECT ON DATABASE \"wamn-db-acme\" TO \"wamn_app\"")
            .expect("grant app role");
        assert!(revoke < grant);
    }

    #[test]
    fn grant_connect_on_database_targets_an_arbitrary_db_name() {
        // The per-project-env path (wamn-q3n.7) passes a full triple-derived name.
        let sql = grant_connect_on_database_sql("wamn-db-acme--billing--dev");
        assert!(sql.contains(
            "REVOKE CONNECT, TEMPORARY ON DATABASE \"wamn-db-acme--billing--dev\" FROM PUBLIC"
        ));
        assert!(
            sql.contains(
                "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_app\""
            )
        );
        // The project-taking 2.3 wrapper delegates to it with the derived name.
        assert_eq!(
            grant_connect_sql("acme"),
            grant_connect_on_database_sql("wamn-db-acme")
        );
    }

    #[test]
    fn writer_acl_roles_are_stable_nologin_and_own_no_grants_here() {
        let sql = ensure_effect_writer_acl_role_sql();
        assert!(sql.contains("'wamn_effect_writer'"));
        assert!(sql.contains("'wamn_run_projection_writer'"));
        assert!(sql.contains("CREATE ROLE %I NOLOGIN"));
        for attr in [
            "NOSUPERUSER",
            "NOCREATEDB",
            "NOCREATEROLE",
            "NOINHERIT",
            "NOREPLICATION",
            "NOBYPASSRLS",
        ] {
            assert!(sql.contains(attr), "missing {attr}");
        }
        for forbidden in ["CONNECT ON DATABASE", "ON SCHEMA", "ON TABLE"] {
            assert!(
                !sql.contains(forbidden),
                "provisioning stole schema-control grant ownership"
            );
        }
    }

    #[test]
    fn generation_prepare_has_only_login_membership_and_project_connect() {
        let role = "wamn_effect_writer_1111111111111111111111111111111111111111_a";
        let sql = prepare_effect_writer_generation_sql(
            "wamn-db-acme--billing--dev",
            role,
            "a'b",
            "2026-09-01T00:00:00Z",
        );
        assert!(sql.contains(&format!("CREATE ROLE \"{role}\" NOLOGIN")));
        assert!(sql.contains(&format!(
            "ALTER ROLE \"{role}\" LOGIN PASSWORD 'a''b' VALID UNTIL '2026-09-01T00:00:00Z'"
        )));
        assert!(sql.contains(&format!("GRANT \"wamn_effect_writer\" TO \"{role}\"")));
        assert!(sql.contains(&format!(
            "GRANT \"wamn_run_projection_writer\" TO \"{role}\""
        )));
        assert!(sql.contains("WITH ADMIN FALSE, INHERIT TRUE, SET TRUE"));
        assert!(sql.contains(&format!(
            "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"{role}\""
        )));
        for forbidden in [
            "SUPERUSER",
            "CREATEDB",
            "CREATEROLE",
            "REPLICATION",
            "BYPASSRLS",
        ] {
            assert!(sql.contains(&format!("NO{forbidden}")));
        }
        assert!(!sql.contains("GRANT USAGE"));
        assert!(!sql.contains("GRANT SELECT"));
        assert!(!sql.contains("GRANT INSERT"));
    }

    #[test]
    fn generation_retire_commits_authority_removal_before_session_termination() {
        let role = "wamn_effect_writer_1111111111111111111111111111111111111111_a";
        let sql = retire_effect_writer_generation_sql("wamn-db-acme--billing--dev", role);
        let membership = sql
            .find("REVOKE \"wamn_effect_writer\", \"wamn_run_projection_writer\"")
            .unwrap();
        let connect = sql.find("REVOKE CONNECT ON DATABASE").unwrap();
        let no_login = sql.find("NOLOGIN PASSWORD NULL").unwrap();
        assert!(membership < connect && connect < no_login);
        assert!(!sql.contains("pg_terminate_backend"));

        let terminate = terminate_effect_writer_generation_sessions_sql(role);
        assert!(terminate.contains(&format!("WHERE usename = '{role}'")));
        assert!(terminate.contains("pid <> pg_backend_pid()"));
        for authority_change in ["REVOKE ", "ALTER ROLE"] {
            assert!(!terminate.contains(authority_change));
        }
    }

    #[test]
    fn generation_state_probe_is_read_only_and_parameterized() {
        let sql = effect_writer_generation_state_sql();
        assert!(sql.starts_with("SELECT"));
        assert!(sql.contains("rolcanlogin"));
        assert!(sql.contains("FROM pg_catalog.pg_authid r"));
        assert!(sql.contains("r.rolpassword IS NOT NULL AS password_set"));
        assert!(sql.contains("pg_auth_members"));
        assert!(sql.contains("NOT m.admin_option AND m.inherit_option AND m.set_option"));
        assert!(sql.contains("AS generation_children_exact"));
        assert!(sql.contains("'wamn_effect_writer', 'wamn_run_projection_writer'"));
        assert!(sql.contains("WHERE edge.roleid = generation.oid"));
        assert!(sql.contains("dependency.deptype = 'o'"));
        assert!(sql.contains("current_database(), 'CONNECT')) > 2"));
        assert!(sql.contains("count(DISTINCT substring("));
        assert!(sql.contains("WHERE m.roleid = r.oid"));
        assert!(sql.contains("rolvaliduntil"));
        assert!(sql.contains("isfinite"));
        assert!(sql.contains("aclexplode"));
        assert!(sql.contains("pg_stat_activity"));
        assert!(sql.contains("r.rolname = $1"));
        for mutation in ["ALTER ", "GRANT ", "REVOKE ", "UPDATE ", "DELETE "] {
            assert!(!sql.contains(mutation));
        }
    }

    #[test]
    fn generation_acl_inventory_covers_cluster_and_database_object_classes() {
        let sql = role_database_acl_inventory_sql();
        for catalog in [
            "pg_database",
            "pg_namespace",
            "pg_class",
            "pg_proc",
            "pg_attribute",
            "pg_type",
            "pg_language",
            "pg_largeobject_metadata",
            "pg_foreign_data_wrapper",
            "pg_foreign_server",
            "pg_tablespace",
            "pg_parameter_acl",
            "pg_default_acl",
        ] {
            assert!(sql.contains(catalog), "missing direct ACL class {catalog}");
        }
        assert!(sql.contains("x.grantee = (SELECT oid FROM wanted)"));
    }

    #[test]
    fn generation_cluster_probes_are_read_only_and_scope_locked() {
        assert!(public_connect_databases_sql().starts_with("SELECT"));
        assert!(public_connect_databases_sql().contains("acl.grantee = 0"));
        assert!(public_connect_databases_sql().contains("privilege_type = 'CONNECT'"));
        assert!(!public_connect_databases_sql().contains("TEMPORARY"));
        assert!(revoke_public_connect_floor_sql().starts_with("DO $$"));
        assert!(revoke_public_connect_floor_sql().contains("NOT datistemplate"));
        assert!(revoke_public_connect_floor_sql().contains("REVOKE CONNECT ON DATABASE %I"));
        assert!(!revoke_public_connect_floor_sql().contains("TEMPORARY"));
        assert!(public_temporary_on_current_database_sql().starts_with("SELECT EXISTS"));
        assert!(public_temporary_on_current_database_sql().contains("current_database()"));
        assert!(public_temporary_on_current_database_sql().contains("'TEMPORARY'"));
        assert!(non_template_databases_sql().contains("NOT datistemplate"));
        assert!(artifact_reader_public_connect_databases_sql().contains("d.datallowconn"));
        assert!(artifact_reader_revoke_public_connect_floor_sql().contains("WHERE datallowconn"));
        assert_eq!(
            effect_writer_scope_lock_sql(),
            "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
        );
    }

    #[test]
    fn artifact_reader_stable_install_is_exactly_tenant_scoped_read_only() {
        let stable = "wamn_artifact_reader_t_1111111111111111111111111111111111111111";
        let policy = "artifact_reader_t_1111111111111111111111111111111111111111";
        let marker = r#"{"kind":"tenant-authority","tenant-id":"tenant'a"}"#;
        let sql = install_artifact_reader_tenant_role_sql(stable, policy, "tenant'a", marker);

        assert!(sql.contains(&format!("CREATE ROLE \"{stable}\" NOLOGIN")));
        assert!(sql.starts_with("REVOKE ALL ON SCHEMA public FROM PUBLIC;"));
        assert_eq!(
            artifact_reader_revoke_public_schema_floor_sql(),
            "REVOKE ALL ON SCHEMA public FROM PUBLIC;"
        );
        for attribute in [
            "NOSUPERUSER",
            "NOCREATEDB",
            "NOCREATEROLE",
            "NOINHERIT",
            "NOREPLICATION",
            "NOBYPASSRLS",
        ] {
            assert!(sql.contains(attribute), "missing {attribute}");
        }
        assert!(sql.contains(&format!(
            "COMMENT ON ROLE \"{stable}\" IS '{{\"kind\":\"tenant-authority\",\"tenant-id\":\"tenant''a\"}}'"
        )));
        assert!(sql.contains(&format!("GRANT USAGE ON SCHEMA catalog TO \"{stable}\"")));
        assert!(sql.contains(&format!(
            "GRANT SELECT (tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length) ON TABLE catalog.execution_bundles TO \"{stable}\""
        )));
        assert!(sql.contains(&format!(
            "CREATE POLICY \"{policy}\" ON catalog.execution_bundles AS RESTRICTIVE FOR SELECT TO \"{stable}\" USING (tenant_id = 'tenant''a')"
        )));
        assert!(!sql.contains("created_at"));
        assert!(!sql.contains("GRANT SELECT ON TABLE"));
        for forbidden in [
            "GRANT INSERT",
            "GRANT UPDATE",
            "GRANT DELETE",
            "GRANT EXECUTE",
        ] {
            assert!(!sql.contains(forbidden), "unexpected {forbidden}");
        }
    }

    #[test]
    fn artifact_reader_generation_prepare_and_retire_preserve_exact_order() {
        let stable = "wamn_artifact_reader_t_1111111111111111111111111111111111111111";
        let generation = "wamn_artifact_reader_2222222222222222222222222222222222222222_a";
        let marker = r#"{"kind":"generation","tenant-id":"tenant'a"}"#;
        let prepare = prepare_artifact_reader_generation_sql(
            "wamn-control",
            stable,
            generation,
            "tenant'a",
            "p'ass",
            "2026-09-01T00:00:00Z",
            marker,
        );
        assert!(prepare.contains(&format!(
            "CREATE ROLE \"{generation}\" NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS"
        )));
        assert!(prepare.contains(&format!(
            "COMMENT ON ROLE \"{generation}\" IS '{{\"kind\":\"generation\",\"tenant-id\":\"tenant''a\"}}'"
        )));
        assert!(prepare.contains(&format!(
            "ALTER ROLE \"{generation}\" LOGIN PASSWORD 'p''ass' VALID UNTIL '2026-09-01T00:00:00Z'"
        )));
        assert!(prepare.contains(&format!(
            "GRANT \"{stable}\" TO \"{generation}\" WITH ADMIN FALSE, INHERIT TRUE, SET FALSE"
        )));
        assert!(prepare.contains(&format!(
            "ALTER ROLE \"{generation}\" IN DATABASE \"wamn-control\" SET app.tenant TO 'tenant''a'"
        )));
        let temporary = prepare
            .find("REVOKE TEMPORARY ON DATABASE \"wamn-control\" FROM PUBLIC")
            .expect("control database TEMPORARY floor");
        let connect = prepare
            .find(&format!(
                "GRANT CONNECT ON DATABASE \"wamn-control\" TO \"{generation}\""
            ))
            .expect("generation CONNECT");
        assert!(temporary < connect);
        assert!(!prepare.contains("REVOKE CONNECT ON DATABASE \"wamn-control\" FROM PUBLIC"));
        assert!(!prepare.contains("GRANT USAGE"));
        assert!(!prepare.contains("GRANT SELECT"));

        let retire = retire_artifact_reader_generation_sql("wamn-control", stable, generation);
        let membership = retire
            .find(&format!("REVOKE \"{stable}\" FROM \"{generation}\""))
            .unwrap();
        let connect = retire.find("REVOKE CONNECT ON DATABASE").unwrap();
        let setting = retire.find("RESET app.tenant").unwrap();
        let login = retire
            .find("NOLOGIN PASSWORD NULL VALID UNTIL 'epoch'")
            .unwrap();
        assert!(membership < connect && connect < setting && setting < login);
        assert!(!retire.contains("COMMENT ON ROLE"));
        assert!(!retire.contains("pg_terminate_backend"));

        let terminate = terminate_artifact_reader_generation_sessions_sql(generation);
        assert!(terminate.contains(&format!("WHERE usename = '{generation}'")));
        assert!(terminate.contains("pid <> pg_backend_pid()"));
        assert!(!terminate.contains("REVOKE "));
        assert!(!terminate.contains("ALTER ROLE"));
    }

    #[test]
    fn artifact_reader_state_queries_expose_every_drift_dimension_read_only() {
        let stable = artifact_reader_tenant_role_state_sql();
        for anchor in [
            "FROM pg_catalog.pg_authid r",
            "shobj_description",
            "AS memberships",
            "AS member_roles",
            "NOT m.set_option",
            "AS member_options_exact",
            "AS owned_objects",
            "r.rolname=$1",
        ] {
            assert!(
                stable.contains(anchor),
                "missing stable-state anchor {anchor}"
            );
        }

        let generation = artifact_reader_generation_state_sql();
        for anchor in [
            "FROM pg_catalog.pg_authid r",
            "shobj_description",
            "AS memberships",
            "NOT m.set_option",
            "AS membership_options_exact",
            "AS member_roles",
            "pg_db_role_setting",
            "LEFT JOIN pg_database",
            "COALESCE(d.datname::text, '*')",
            "unnest(s.setconfig)",
            "AS database_settings",
            "aclexplode(d.datacl)",
            "acl.privilege_type='CONNECT'",
            "AS connect_databases",
            "pg_stat_activity",
            "AS sessions",
            "AS owned_objects",
            "r.rolname=$1",
        ] {
            assert!(
                generation.contains(anchor),
                "missing generation-state anchor {anchor}"
            );
        }
        for query in [stable, generation] {
            assert!(query.starts_with("SELECT"));
            for mutation in ["ALTER ", "GRANT ", "REVOKE ", "UPDATE ", "DELETE "] {
                assert!(
                    !query.contains(mutation),
                    "state query mutates with {mutation}"
                );
            }
        }
    }

    #[test]
    fn artifact_reader_policy_and_session_queries_are_exact_and_parameterized() {
        let rls = artifact_reader_rls_state_sql();
        assert!(rls.starts_with("SELECT"));
        assert!(rls.contains("c.relrowsecurity, c.relforcerowsecurity"));
        assert!(rls.contains("'catalog.execution_bundles'::regclass"));

        let policy = artifact_reader_policy_state_sql();
        assert!(policy.starts_with("SELECT"));
        assert!(policy.contains("p.polrelid='catalog.execution_bundles'::regclass"));
        assert!(policy.contains("0::oid=ANY(p.polroles)"));
        assert!(policy.contains("rolname=$1"));
        assert!(policy.contains("rolname=$2"));
        assert!(policy.contains("p.polname=$3"));
        assert!(policy.contains("p.polcmd::text"));
        assert!(policy.contains("p.polpermissive"));
        assert!(policy.contains("pg_get_expr(p.polqual,p.polrelid,false)"));
        assert!(policy.contains("pg_get_expr(p.polwithcheck,p.polrelid,false)"));
        assert!(policy.contains("ORDER BY p.polname"));

        let sessions = artifact_reader_generation_sessions_sql();
        assert_eq!(
            sessions,
            "SELECT pid, datname, application_name, state \
               FROM pg_catalog.pg_stat_activity WHERE usename=$1 ORDER BY pid"
        );
        assert!(!sessions.contains("query"));
        assert_eq!(
            artifact_reader_replacement_use_sql(),
            "SELECT count(*)::bigint FROM pg_catalog.pg_stat_activity \
              WHERE usename=$1 AND datname=$2 AND application_name=$3"
        );
    }

    #[test]
    fn artifact_reader_effective_and_public_authority_queries_cover_the_full_app_surface() {
        let effective = artifact_reader_effective_authority_sql();
        assert!(effective.starts_with("WITH wanted AS"));
        assert!(effective.contains("nspname <> 'information_schema'"));
        assert!(effective.contains("nspname !~ '^pg_'"));
        for predicate in [
            "has_database_privilege",
            "has_schema_privilege",
            "has_table_privilege",
            "has_column_privilege",
            "has_sequence_privilege",
            "has_function_privilege",
        ] {
            assert!(effective.contains(predicate), "missing {predicate}");
        }
        for privilege in [
            "'CREATE'",
            "'INSERT'",
            "'UPDATE'",
            "'DELETE'",
            "'TRUNCATE'",
            "'REFERENCES'",
            "'TRIGGER'",
            "'EXECUTE'",
        ] {
            assert!(effective.contains(privilege), "missing {privilege}");
        }
        assert!(effective.contains("FROM wanted w"));

        let public = artifact_reader_public_authority_sql();
        assert!(public.starts_with("WITH app_schemas AS"));
        assert!(public.contains("nspname <> 'information_schema'"));
        assert!(public.contains("nspname !~ '^pg_'"));
        assert_eq!(public.matches("x.grantee=0").count(), 12);
        assert!(public.contains("acldefault('f', p.proowner)"));
        assert!(public.contains("acldefault('T', t.typowner)"));
        assert!(public.contains("acldefault('l', l.lanowner)"));
        assert!(public.contains("pg_largeobject_metadata"));
        assert!(public.contains("pg_foreign_data_wrapper"));
        assert!(public.contains("pg_foreign_server"));
        assert!(public.contains("pg_tablespace"));
        assert!(public.contains("pg_parameter_acl"));
        assert!(public.contains("a.attacl"));
        assert!(public.contains("pg_default_acl"));
        assert!(public.contains("d.deptype='e'"));
    }

    #[test]
    fn artifact_reader_scope_lock_and_unlock_use_one_parameterized_key() {
        assert_eq!(
            artifact_reader_scope_lock_sql(),
            "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
        );
        assert_eq!(
            artifact_reader_scope_unlock_sql(),
            "SELECT pg_advisory_unlock(hashtextextended($1::text, 0))"
        );
    }

    #[test]
    fn replication_role_is_replication_login_and_otherwise_least_privilege() {
        let sql = ensure_replication_role_sql("wamn_cdc_acme__billing__dev", "s3cr3t");
        assert!(sql.contains("IF NOT EXISTS"), "idempotent guard");
        assert!(sql.contains("CREATE ROLE \"wamn_cdc_acme__billing__dev\" LOGIN REPLICATION"));
        assert!(sql.contains("PASSWORD 's3cr3t'"));
        // The R8b tier: REPLICATION but nothing else elevated.
        for attr in ["NOSUPERUSER", "NOCREATEDB", "NOCREATEROLE", "NOBYPASSRLS"] {
            assert!(sql.contains(attr), "missing {attr}");
        }
        // A password with a quote is escaped, not injected.
        assert!(ensure_replication_role_sql("r", "a'b").contains("PASSWORD 'a''b'"));
    }

    #[test]
    fn publication_covers_the_schema_and_is_idempotent() {
        let sql = create_publication_sql("wamn_cdc_acme__billing__dev", "app");
        // FOR TABLES IN SCHEMA (auto-includes tables created later) — never the
        // per-table form and never FOR ALL TABLES (which would leak app_system
        // and any other schema into the stream).
        assert!(sql.contains(
            "CREATE PUBLICATION \"wamn_cdc_acme__billing__dev\" FOR TABLES IN SCHEMA \"app\""
        ));
        assert!(!sql.contains("FOR ALL TABLES"));
        // Idempotent: guarded by a pg_publication probe (no IF NOT EXISTS in PG).
        assert!(sql.contains("IF NOT EXISTS (SELECT FROM pg_publication WHERE pubname = 'wamn_cdc_acme__billing__dev')"));
        // The eager schema guard is a separate statement.
        assert_eq!(
            ensure_schema_sql("app"),
            "CREATE SCHEMA IF NOT EXISTS \"app\""
        );
    }

    #[test]
    fn failover_slot_uses_the_sql_function_form_with_failover_true() {
        let sql = create_failover_slot_sql("wamn_cdc_acme__billing__dev");
        // The PG17+ five-argument form: (slot, plugin, temporary, twophase,
        // FAILOVER) — pgoutput, non-temporary, no two-phase, failover=true, the
        // exact shape pg_walstream's ensure_replication_slot tolerates.
        assert!(sql.contains(
            "pg_create_logical_replication_slot('wamn_cdc_acme__billing__dev', 'pgoutput', false, false, true)"
        ));
        // Idempotent: guarded by a pg_replication_slots probe.
        assert!(
            sql.contains("IF NOT EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = 'wamn_cdc_acme__billing__dev')")
        );
    }

    #[test]
    fn replication_grants_cover_connect_usage_and_current_tables() {
        let sql = grant_replication_access_sql("wamn-db-acme--billing--dev", "wamn_cdc_x", "app");
        assert!(sql.contains(
            "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_cdc_x\""
        ));
        assert!(sql.contains("GRANT USAGE ON SCHEMA \"app\" TO \"wamn_cdc_x\""));
        assert!(sql.contains("GRANT SELECT ON ALL TABLES IN SCHEMA \"app\" TO \"wamn_cdc_x\""));
    }

    /// The entity-map drift guard (wamn-l5i9.11): the PINNED SQL is the
    /// load-bearing contract — the reader's OID lookup, the same-transaction
    /// upsert, and the OID-keyed rename-proofing all ride these exact strings.
    #[test]
    fn entity_map_is_oid_keyed_and_upserted_from_pg_class() {
        assert_eq!(
            ensure_entity_map_sql("app"),
            "CREATE TABLE IF NOT EXISTS \"app\".wamn_entities ( \
               relation_oid oid PRIMARY KEY, \
               entity_id text NOT NULL, \
               table_name text NOT NULL)"
        );
        let upsert = upsert_entity_map_sql("app");
        // The OID is resolved server-side from pg_class IN the DDL transaction
        // (ordinary tables only), keyed for conflict on relation_oid — a
        // rename re-upserts the SAME row (new table_name, same entity/OID).
        assert!(
            upsert.contains(
                "INSERT INTO \"app\".wamn_entities (relation_oid, entity_id, table_name)"
            )
        );
        // `$2::text` in BOTH the projection and the WHERE — a bare `$2` would
        // be deduced `name` at `c.relname = $2` and `text` at the column,
        // which tokio_postgres rejects ("inconsistent types deduced").
        assert!(upsert.contains("SELECT c.oid, $1, $2::text FROM pg_class c"));
        assert!(
            upsert.contains("WHERE n.nspname = 'app' AND c.relname = $2::text AND c.relkind = 'r'")
        );
        assert!(upsert.contains("ON CONFLICT (relation_oid) DO UPDATE"));
        assert!(
            upsert.contains("SET entity_id = EXCLUDED.entity_id, table_name = EXCLUDED.table_name")
        );
    }

    #[test]
    fn cdc_teardown_builders_are_guarded() {
        assert_eq!(
            drop_publication_sql("wamn_cdc_x"),
            "DROP PUBLICATION IF EXISTS \"wamn_cdc_x\""
        );
        let drop_slot = drop_replication_slot_sql("wamn_cdc_x");
        assert!(drop_slot.contains("pg_drop_replication_slot('wamn_cdc_x')"));
        assert!(drop_slot.contains("IF EXISTS"));
    }

    #[test]
    fn database_exists_is_parameterized() {
        assert_eq!(
            database_exists_sql(),
            "SELECT EXISTS (SELECT FROM pg_database WHERE datname = $1)"
        );
    }

    /// The escapers are inlined to keep the prod dep closure at `serde_json`, but
    /// they MUST stay byte-identical to the canonical `wamn_schema_compiler::sql::*` that back
    /// the injection-safety argument (a slug cannot contain a `"`, so the derived
    /// database/role DDL is safe). Assert over adversarial inputs plus an
    /// exhaustive single-ASCII-char sweep so any future divergence in either copy
    /// fails here. (`wamn-schema-compiler` is a dev-dependency, so this costs the prod build
    /// nothing.)
    #[test]
    fn inlined_escapers_match_canonical_wamn_schema_compiler() {
        let mut cases: Vec<String> = vec![
            "".into(),
            "a".into(),
            "plain_ident".into(),
            "a\"b".into(),
            "\"\"".into(),
            "a'b".into(),
            "''".into(),
            "a\"'b".into(),
            "back\\slash".into(),
            "tab\there".into(),
            "new\nline".into(),
            "nul\0byte".into(),
            "münz".into(),
            "wamn-db-acme--billing--dev".into(),
            "'; DROP TABLE x; --".into(),
        ];
        for c in 0u8..=0x7f {
            cases.push(format!("x{}y", c as char));
        }
        for s in &cases {
            assert_eq!(
                quote_ident(s),
                wamn_pg_core::quote_ident(s),
                "quote_ident drift on {s:?}"
            );
            assert_eq!(
                quote_literal(s),
                wamn_pg_core::quote_literal(s),
                "quote_literal drift on {s:?}"
            );
        }
    }
}
