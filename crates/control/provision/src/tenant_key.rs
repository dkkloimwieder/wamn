//! The pure derivation guest RLS uses to reach a tenant from `current_user`.
//!
//! # Why a function and not a mapping table (`wamn-0h0g.22.6`, option (c))
//!
//! Guest tenant authority must come from `current_user`, because a GUC claim is
//! settable by whoever holds the session. The obvious replacement — a table
//! mapping login to tenant — is banned, and the ban is precise: it targets
//! MUTABLE, SETTABLE STATE between identity and authority. A fixed derivation
//! with no writer and no state is computation, not indirection.
//!
//! # Why the digest is not implemented here
//!
//! [`tenant_key`] is a THIN WRAPPER over [`workload_role_scope_hash`]. A second
//! Rust implementation would be the one drift this construction cannot absorb:
//! the role name minted by provisioning and the key computed by the predicate
//! must be the same string, or every guest read refuses. There is exactly one
//! Rust definition, and the SQL below is proven equal to it by a live test.
//!
//! # Why the database is baked in as a literal
//!
//! `current_database()` is `STABLE`, not `IMMUTABLE`, so reading it at call
//! time would make this function unusable in an expression index — destroying
//! the sargability that option (c) depends on. It also cannot be dropped from
//! the preimage: without it, one tenant in two project-environment databases
//! derives the same key and collides on a single role name.

use wamn_pg_core::{quote_ident, quote_literal};

use crate::workload_role::{WorkloadRoleFamily, WorkloadRoleScope, workload_role_scope_hash};
use crate::{APP_ROLE, DB_OWNER_ROLE};

/// Schema hosting authority derivations inside a project-environment database.
///
/// Same name as the control plane's `wamn_authority`, and deliberately a
/// SEPARATE OBJECT in a separate database — the two-plane residency pattern.
pub const TENANT_KEY_SCHEMA: &str = "wamn_authority";

/// The schema-qualified function name.
///
/// Every predicate MUST use this qualified form. An unqualified call resolves
/// through the caller's `search_path`, which is the settable hijack the whole
/// design exists to remove.
pub const TENANT_KEY_FUNCTION: &str = "wamn_authority.tenant_key";

/// The tenant key for one tenant in one project-environment database.
///
/// Equal by construction to the scope digest provisioning mints role names
/// from, because it IS that function.
pub fn tenant_key(tenant: &str, database: &str) -> String {
    workload_role_scope_hash(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant { tenant, database },
    )
    .expect("App's scope kind is Tenant, so a Tenant scope cannot mismatch it")
}

/// The DDL creating the derivation in one project-environment database.
///
/// Idempotent by construction (`CREATE OR REPLACE`, `IF NOT EXISTS`), which is
/// what lets it converge on `wamn-0h0g.11.49`'s path for existing databases
/// while `catalog-schema.sql` carries fresh installs. Idempotence is not the
/// proof, though: the converge arm asserts POST-STATE — the definition digest,
/// the `IMMUTABLE`/`PARALLEL SAFE` flags read from `pg_proc`, and exact grants.
///
/// `SET search_path = pg_catalog` pins every builtin this body calls, so the
/// derivation cannot be redirected by a caller's search path.
pub fn tenant_key_function_sql(database: &str) -> String {
    let domain = WorkloadRoleFamily::App.scope_domain();
    let domain_text = std::str::from_utf8(domain).expect("the scope domain is ASCII");
    format!(
        "CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION {owner};\n\
         REVOKE ALL ON SCHEMA {schema} FROM PUBLIC;\n\
         CREATE OR REPLACE FUNCTION {schema}.tenant_key(tenant text)\n\
         RETURNS text\n\
         LANGUAGE sql\n\
         IMMUTABLE\n\
         PARALLEL SAFE\n\
         STRICT\n\
         SET search_path = pg_catalog\n\
         AS $$\n    \
         SELECT substr(encode(sha256(\n           \
         int8send({domain_len}::bigint) || convert_to({domain_lit}, 'UTF8')\n        \
         || int8send({tenant_tag_len}::bigint) || convert_to('tenant', 'UTF8')\n        \
         || int8send(octet_length(convert_to(tenant, 'UTF8'))::bigint) \
         || convert_to(tenant, 'UTF8')\n        \
         || int8send({database_tag_len}::bigint) || convert_to('database', 'UTF8')\n        \
         || int8send({database_len}::bigint) || convert_to({database_lit}, 'UTF8')\n       \
         ), 'hex'), 1, {hex_len})\n\
         $$;\n\
         ALTER FUNCTION {schema}.tenant_key(text) OWNER TO {owner};\n\
         REVOKE ALL ON FUNCTION {schema}.tenant_key(text) FROM PUBLIC;\n\
         GRANT USAGE ON SCHEMA {schema} TO {app};\n\
         GRANT EXECUTE ON FUNCTION {schema}.tenant_key(text) TO {app};",
        schema = quote_ident(TENANT_KEY_SCHEMA),
        owner = quote_ident(DB_OWNER_ROLE),
        app = quote_ident(APP_ROLE),
        domain_len = domain.len(),
        domain_lit = quote_literal(domain_text),
        tenant_tag_len = "tenant".len(),
        database_tag_len = "database".len(),
        database_len = database.len(),
        database_lit = quote_literal(database),
        hex_len = crate::workload_role::SCOPE_HASH_HEX_LEN,
    )
}

#[cfg(test)]
mod tests {
    use super::{TENANT_KEY_FUNCTION, tenant_key, tenant_key_function_sql};
    use crate::workload_role::{WorkloadRoleFamily, WorkloadRoleScope, workload_role_scope_hash};

    /// The single sharpest failure mode in this construction: if the predicate's
    /// key and the minted role name disagree, EVERY guest read refuses. They
    /// cannot disagree, because there is one Rust definition and this asserts
    /// the wrapper is it.
    #[test]
    fn the_tenant_key_is_the_workload_scope_digest_and_not_a_second_one() {
        let expected = workload_role_scope_hash(
            WorkloadRoleFamily::App,
            WorkloadRoleScope::Tenant {
                tenant: "acme",
                database: "wamn-db-acme--billing--dev",
            },
        )
        .expect("App takes a tenant scope");
        assert_eq!(tenant_key("acme", "wamn-db-acme--billing--dev"), expected);
    }

    /// Two project-environment databases must not derive one key for one
    /// tenant, or their guest logins collide on a single role name.
    #[test]
    fn the_same_tenant_derives_a_different_key_per_database() {
        assert_ne!(
            tenant_key("acme", "wamn-db-acme--billing--dev"),
            tenant_key("acme", "wamn-db-acme--billing--prod"),
        );
    }

    /// GENERATED SQL FROM A RUST BUILDER, so the string is pinned: it is the
    /// only thing that catches the builder moving, and this builder's output is
    /// a security boundary.
    #[test]
    fn the_emitted_ddl_is_frozen() {
        let sql = tenant_key_function_sql("wamn-db-acme--billing--dev");
        assert_eq!(
            sql,
            "CREATE SCHEMA IF NOT EXISTS \"wamn_authority\" AUTHORIZATION \"wamn_db_owner\";\n\
             REVOKE ALL ON SCHEMA \"wamn_authority\" FROM PUBLIC;\n\
             CREATE OR REPLACE FUNCTION \"wamn_authority\".tenant_key(tenant text)\n\
             RETURNS text\n\
             LANGUAGE sql\n\
             IMMUTABLE\n\
             PARALLEL SAFE\n\
             STRICT\n\
             SET search_path = pg_catalog\n\
             AS $$\n    \
             SELECT substr(encode(sha256(\n           \
             int8send(19::bigint) || convert_to('wamn.app.scope.v0.1', 'UTF8')\n        \
             || int8send(6::bigint) || convert_to('tenant', 'UTF8')\n        \
             || int8send(octet_length(convert_to(tenant, 'UTF8'))::bigint) \
             || convert_to(tenant, 'UTF8')\n        \
             || int8send(8::bigint) || convert_to('database', 'UTF8')\n        \
             || int8send(26::bigint) || convert_to('wamn-db-acme--billing--dev', 'UTF8')\n       \
             ), 'hex'), 1, 40)\n\
             $$;\n\
             ALTER FUNCTION \"wamn_authority\".tenant_key(text) OWNER TO \"wamn_db_owner\";\n\
             REVOKE ALL ON FUNCTION \"wamn_authority\".tenant_key(text) FROM PUBLIC;\n\
             GRANT USAGE ON SCHEMA \"wamn_authority\" TO \"wamn_app\";\n\
             GRANT EXECUTE ON FUNCTION \"wamn_authority\".tenant_key(text) TO \"wamn_app\";"
        );
    }

    /// The qualified name is the contract every predicate must use.
    #[test]
    fn the_function_name_is_schema_qualified() {
        assert_eq!(TENANT_KEY_FUNCTION, "wamn_authority.tenant_key");
        assert!(
            TENANT_KEY_FUNCTION.contains('.'),
            "an unqualified name would resolve through the caller's search_path"
        );
    }
}
