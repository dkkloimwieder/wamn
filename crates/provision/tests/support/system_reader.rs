use std::fmt::Write as _;

pub use super::surface::{psql, role_url, run_admin};

use wamn_control_provision::{SYSTEM_SCHEMA_SQL, SystemReader, system_reader_login};

pub const ORG: &str = "acme";
pub const PROJECT: &str = "widgets";
pub const ENV: &str = "dev";
pub const READER_PW: &str = "wamn_system_reader_pw";

pub fn generation_role(reader: SystemReader, database: &str) -> wamn_identity_protocol::LoginName {
    system_reader_login(reader, ORG, PROJECT, &ENV.parse().unwrap(), database)
}

/// Drop every role this acceptance test mints, so a leftover HEALTHY role cannot satisfy
/// the builder's `IF NOT EXISTS` and mask a mutated one.
///
/// Roles are CLUSTER-wide, so re-creating the schemas does not reach them.
/// `DROP OWNED BY` precedes `DROP ROLE` inside an existence test: a privilege
/// left behind in this database would otherwise refuse the drop outright.
pub fn reset_roles(admin_url: &str, database: &str) {
    let mut roles = Vec::new();
    for reader in SystemReader::ALL {
        roles.push(generation_role(reader, database).to_string());
        roles.push(reader.family().acl_role().to_owned());
    }
    let mut script = String::new();
    for role in roles {
        writeln!(
            script,
            "DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $reset$;"
        )
        .expect("writing to a String cannot fail");
    }
    run_admin(admin_url, "reset leftover reader roles", &script);
}

/// Apply `deploy/sql/system-schema.sql` under its real `wamn_system` owner.
pub fn apply_system_schema(admin_url: &str) {
    let mut script = String::from(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
           CREATE ROLE wamn_system LOGIN PASSWORD 'wamn_system' NOSUPERUSER; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS registry CASCADE;\n\
         DROP SCHEMA IF EXISTS provisioning CASCADE;\n\
         DROP SCHEMA IF EXISTS identity CASCADE;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
           current_database()); END $$;\n",
    );
    script.push_str(wamn_control_provision::sql::ensure_db_owner_role_sql());
    script.push_str("\nSET ROLE wamn_system;\n");
    script.push_str(SYSTEM_SCHEMA_SQL);
    script.push_str("\nRESET ROLE;\n");
    run_admin(admin_url, "apply the system schema", &script);
}

/// Read whether any privilege permits delegation to another role.
pub const ROLE_GRANTS_SQL: &str = "SELECT grantable FROM ( \
           SELECT 'relation' AS type, n.nspname::text AS sch, c.relname::text AS obj, \
                  x.privilege_type::text AS priv, x.is_grantable AS grantable \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL aclexplode(c.relacl) x \
            WHERE x.grantee = $1::text::regrole \
           UNION ALL \
           SELECT 'column', n.nspname::text, c.relname || '.' || a.attname, \
                  x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL aclexplode(a.attacl) x \
            WHERE x.grantee = $1::text::regrole \
           UNION ALL \
           SELECT 'schema', n.nspname::text, n.nspname::text, \
                  x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) x \
            WHERE x.grantee = $1::text::regrole \
           UNION ALL \
           SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
             CROSS JOIN LATERAL aclexplode(p.proacl) x WHERE x.grantee=$1::text::regrole \
           UNION ALL \
           SELECT 'database', '', d.datname::text, x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) x \
            WHERE x.grantee = $1::text::regrole \
         ) q ORDER BY 1;";

pub fn psql_tuples(url: &str, statement: &str) -> (bool, String, String) {
    psql(url, statement)
}

/// Run one statement in a session authenticated AS the reader and return the
/// SQLSTATE it raised, or `None` when it succeeded.
pub fn reader_sqlstate(reader_url: &str, statement: &str) -> Option<String> {
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    super::surface::sqlstate(
        reader_url,
        &format!(
            "BEGIN; INSERT INTO platform.tx_identity(principal_id,role,application) VALUES('{actor}','','fixture'); {statement}; COMMIT;"
        ),
    )
}
