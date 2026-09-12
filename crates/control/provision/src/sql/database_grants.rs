//! Database connection rules and direct role grants.

/// List connectable databases that grant `CONNECT` to PUBLIC.
///
/// The filter is `datallowconn`, not `NOT datistemplate`, and must stay so:
/// `template1` is a template AND connectable, so a template filter reported a
/// clean floor while `template1` still carried PostgreSQL's default PUBLIC
/// `CONNECT`. `template0` (`datallowconn = false`) is correctly out of scope —
/// it keeps its PUBLIC `CONNECT` aclitem, which no session can use.
pub fn public_connect_databases_sql() -> &'static str {
    "SELECT d.datname::text FROM pg_database d \
      WHERE d.datallowconn AND EXISTS ( \
        SELECT FROM aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl \
         WHERE acl.grantee = 0 AND acl.privilege_type = 'CONNECT') \
      ORDER BY d.datname::text"
}

/// Revoke PUBLIC `CONNECT` from every CONNECTABLE database in this cluster.
///
/// Database privileges are cluster catalog entries, so the DO block may target
/// each database while connected to the exact project database. This gives ctl
/// ownership of converging the ratified cluster-wide floor during initial
/// generation preparation.
///
/// The loop selects on `datallowconn`, **not** `NOT datistemplate`. `template1`
/// is a template with `datallowconn = true`, so a template filter left its
/// default PUBLIC `CONNECT` untouched — and a `template1` session is a live
/// session on the cluster from which any database that principal OWNS can be
/// `ALTER`ed or `DROP`ped, needing no `CONNECT` on the target at all.
/// `template0` is `datallowconn = false` and stays untouched, as it must:
/// `CREATE DATABASE … TEMPLATE template1` does not require the creator to hold
/// `CONNECT`, so closing the route costs no provisioning capability.
pub fn revoke_public_connect_floor_sql() -> &'static str {
    "DO $$ DECLARE database_name text; BEGIN \
       FOR database_name IN SELECT datname FROM pg_database WHERE datallowconn LOOP \
         EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', database_name); \
       END LOOP; \
     END $$;"
}

/// Check PUBLIC's effective `TEMPORARY` privilege on the target.
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

/// All non-template databases available for exact cross-database ACL checks.
///
/// This one keeps the `NOT datistemplate` filter on purpose, unlike
/// [`public_connect_databases_sql`] / [`revoke_public_connect_floor_sql`]: the
/// caller OPENS a session against each name it returns, and an open session on
/// `template1` makes concurrent `CREATE DATABASE … TEMPLATE template1` fail.
/// The floor builders only read and revoke catalog ACLs, so they can and must
/// cover `template1`; this one cannot.
pub fn non_template_databases_sql() -> &'static str {
    "SELECT datname::text FROM pg_database WHERE NOT datistemplate ORDER BY datname::text"
}

/// Read-only direct grants for one role in the connected database.
pub fn role_database_grants_sql() -> &'static str {
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
