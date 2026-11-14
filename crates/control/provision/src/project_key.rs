//! The derivations shared catalog and run policies use for `current_user`.
//! The platform administration tables use the database boundary and call none of them.
//!
//! # Why functions and not a mapping table (`wamn-0h0g.22.6`, option (c))
//!
//! Guest project authority must come from `current_user`, because a GUC identity is
//! settable by whoever holds the session. The obvious replacement — a table
//! mapping login to project — is banned, and the ban is precise: it targets
//! MUTABLE, SETTABLE STATE between identity and authority. A fixed derivation
//! with no writer and no state is computation, not indirection.
//!
//! # The two halves of one predicate
//!
//! [`project_key`] derives a key FROM A ROW; [`current_project_key_pattern`]
//! backs the function that derives the key OF THE CONNECTED ROLE. The governed
//! predicate is the equality of the two:
//!
//! ```sql
//! wamn_authority.project_key(project_env_key) = wamn_authority.current_project_key()
//! ```
//!
//! The left side is the BARE call the expression index is built on, which is
//! the only form PostgreSQL will match that index against — so the right side
//! has to be a scalar, and every shape that wraps the indexed expression is
//! refuted by the same ruling that demands the index.
//!
//! # Why the digest is not implemented here
//!
//! [`project_key`] is a THIN WRAPPER over [`workload_role_identity_hash`]. A second
//! Rust implementation would be the one drift this construction cannot absorb:
//! the role name minted by provisioning and the key computed by the predicate
//! must be the same string, or every guest read refuses. There is exactly one
//! Rust definition, and the SQL below is shown equal to it by a live test.
//!
//! # Why the database is baked in as a literal
//!
//! `current_database()` is `STABLE`, not `IMMUTABLE`, so reading it at call
//! time would make [`project_key`]'s SQL twin unusable in an expression index —
//! destroying the sargability that option (c) depends on. It also cannot be
//! dropped from the preimage: without it, one project in two project-environment
//! databases derives the same key and collides on a single role name.

use postgres_protocol::escape::{escape_identifier, escape_literal};

use crate::APP_ROLE;
use crate::DB_OWNER_ROLE;
use crate::workload_role::{
    IDENTITY_HASH_HEX_LEN, WorkloadRoleFamily, WorkloadRoleIdentity, workload_role_identity_hash,
};

/// Schema that owns the database authority functions.
pub const PROJECT_KEY_SCHEMA: &str = "wamn_authority";
/// Qualified row-key function.
pub const PROJECT_KEY_FUNCTION: &str = "wamn_authority.project_key";

/// The project key for one project in one project-environment database.
///
/// Equal by construction to the identity digest provisioning mints role names
/// from, because it IS that function.
pub fn project_key(project_env_key: &str, database: &str) -> String {
    workload_role_identity_hash(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database,
        },
    )
    .expect("App's identity type is ProjectEnvKey, so a ProjectEnvKey identity cannot mismatch it")
}

/// The anchored pattern that recovers a project key from a guest login name.
///
/// Every part is read from the SAME constants [`workload_login`]
/// composes a role name from: the family's prefix and the digest
/// width. This ensures so the naming decision on
/// `wamn-0h0g.22.6.4` cannot move the mint without moving this pattern with it.
///
/// Anchored at both ends: an unanchored pattern would match a crafted role name
/// that merely CONTAINS a victim's key.
///
/// [`workload_login`]: crate::workload_role::workload_login
pub fn current_project_key_pattern() -> String {
    format!("^(?:wamn_app|wamn_query)_([0-9a-f]{{{IDENTITY_HASH_HEX_LEN}}})$")
}

/// Owner the bootstrap rendering gives the schema and both functions.
///
/// `CURRENT_USER`, not the named platform role. The static DDL files that carry
/// this bootstrap already declare `AUTHORIZATION CURRENT_USER`, and naming a
/// role that may not exist would add a new precondition to every applier —
/// seven live acceptance tests plus the production path in `services/ctl`. The applier IS
/// the platform role in every real deployment, so `CURRENT_USER` satisfies the
/// ownership rider without inventing a requirement.
const BOOTSTRAP_OWNER: &str = "CURRENT_USER";

/// Placeholder the bootstrap rendering substitutes with the quoted database name.
const DATABASE_LITERAL_PLACEHOLDER: &str = "@wamn_database_literal@";

/// Placeholder the bootstrap rendering substitutes with the database name's
/// octet length.
const DATABASE_OCTETS_PLACEHOLDER: &str = "@wamn_database_octets@";

/// The DDL creating both authority derivations in one project-environment
/// database, over pre-rendered database fragments.
///
/// ONE TEMPLATE, TWO RENDERINGS. [`authority_derivations_sql`] substitutes a
/// real database name; [`authority_derivations_bootstrap_sql`] substitutes
/// placeholders the server fills in at apply time. Sharing the template is not
/// enough to establish agreement — a live test applies both and compares the digests
/// `pg_get_functiondef` reports.
///
/// `SET search_path = pg_catalog` pins every builtin these bodies call, so
/// neither derivation can be redirected by a caller's search path.
fn derivations_template(database_literal: &str, database_octets: &str, owner: &str) -> String {
    let domain = WorkloadRoleFamily::App.identity_domain();
    let domain_text = std::str::from_utf8(domain).expect("the identity domain is ASCII");
    format!(
        "CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION {owner};\n\
         REVOKE ALL ON SCHEMA {schema} FROM PUBLIC;\n\
         GRANT USAGE ON SCHEMA {schema} TO {app};\n\
         CREATE OR REPLACE FUNCTION {schema}.project_key(project_env_key text)\n\
         RETURNS text\n\
         LANGUAGE sql\n\
         IMMUTABLE\n\
         PARALLEL SAFE\n\
         STRICT\n\
         SET search_path = pg_catalog\n\
         AS $$\n    \
         SELECT substr(encode(sha256(\n           \
         int8send({domain_len}::bigint) || convert_to({domain_lit}, 'UTF8')\n        \
         || int8send({project_env_key_tag_len}::bigint) || convert_to('project_env_key', 'UTF8')\n        \
         || int8send(octet_length(convert_to(project_env_key, 'UTF8'))::bigint) \
         || convert_to(project_env_key, 'UTF8')\n        \
         || int8send({database_tag_len}::bigint) || convert_to('database', 'UTF8')\n        \
         || int8send({database_octets}::bigint) || convert_to({database_literal}, 'UTF8')\n       \
         ), 'hex'), 1, {hex_len})\n\
         $$;\n\
         ALTER FUNCTION {schema}.project_key(text) OWNER TO {owner};\n\
         REVOKE ALL ON FUNCTION {schema}.project_key(text) FROM PUBLIC;\n\
         GRANT EXECUTE ON FUNCTION {schema}.project_key(text) TO {app};\n\
         CREATE OR REPLACE FUNCTION {schema}.current_project_key()\n\
         RETURNS text\n\
         LANGUAGE sql\n\
         STABLE\n\
         PARALLEL SAFE\n\
         SET search_path = pg_catalog\n\
         AS $$\n    \
         SELECT substring(current_user::text from {pattern_lit})\n\
         $$;\n\
         ALTER FUNCTION {schema}.current_project_key() OWNER TO {owner};\n\
         REVOKE ALL ON FUNCTION {schema}.current_project_key() FROM PUBLIC;\n\
         GRANT EXECUTE ON FUNCTION {schema}.current_project_key() TO {app};",
        schema = escape_identifier(PROJECT_KEY_SCHEMA),
        app = escape_identifier(APP_ROLE),
        domain_len = domain.len(),
        domain_lit = escape_literal(domain_text),
        project_env_key_tag_len = "project_env_key".len(),
        database_tag_len = "database".len(),
        hex_len = IDENTITY_HASH_HEX_LEN,
        pattern_lit = escape_literal(&current_project_key_pattern()),
    )
}

/// The DDL creating both authority derivations for a KNOWN database.
///
/// Idempotent by construction (`CREATE OR REPLACE`, `IF NOT EXISTS`), which is
/// what lets it converge on `wamn-0h0g.11.49`'s path for existing databases
/// while `wamn_catalog::CATALOG_SCHEMA_SQL` carries fresh installs. Idempotence is not the
/// test, though: the converge arm asserts POST-STATE — the definition digest,
/// the volatility and parallel-safety flags read from `pg_proc`, and exact
/// grants.
///
/// Both functions land in ONE builder because both must reach both appliers;
/// two builders would be two places to remember and one place to forget.
pub fn authority_derivations_sql(database: &str) -> String {
    derivations_template(
        &escape_literal(database),
        &database.len().to_string(),
        &escape_identifier(DB_OWNER_ROLE),
    )
}

/// The same DDL for a database whose name is NOT known at authoring time.
///
/// `CATALOG_SCHEMA_SQL` and `app-schema.sql` contain static SQL applied to
/// project-environment databases named per project and environment, so neither
/// can carry the literal [`authority_derivations_sql`] needs. The database
/// cannot simply be read at call time either: `current_database()` is `STABLE`,
/// so a function calling it is not indexable and the whole sargability argument
/// for option (c) collapses.
///
/// So the name is substituted at APPLY time and baked into the body as a
/// literal, leaving the installed function `IMMUTABLE` exactly as the
/// known-database rendering does. `quote_literal` runs on the server, so a
/// database name containing a quote cannot break out of the body.
pub fn authority_derivations_bootstrap_sql() -> String {
    format!(
        "DO $wamn_authority_bootstrap$\n\
         BEGIN\n    \
         EXECUTE replace(replace($wamn_authority_derivations${template}$wamn_authority_derivations$,\n        \
         '{literal}', quote_literal(current_database())),\n        \
         '{octets}', octet_length(convert_to(current_database(), 'UTF8'))::text);\n\
         END\n\
         $wamn_authority_bootstrap$;",
        template = derivations_template(
            DATABASE_LITERAL_PLACEHOLDER,
            DATABASE_OCTETS_PLACEHOLDER,
            BOOTSTRAP_OWNER,
        ),
        literal = DATABASE_LITERAL_PLACEHOLDER,
        octets = DATABASE_OCTETS_PLACEHOLDER,
    )
}

#[cfg(test)]
mod tests {
    use super::project_key;
    use crate::workload_role::{
        WorkloadRoleFamily, WorkloadRoleIdentity, workload_role_identity_hash,
    };

    const DATABASE: &str = "wamn-db-acme--dev--billing";

    /// The single sharpest failure mode in this construction: if the predicate's
    /// key and the minted role name disagree, EVERY guest read refuses. They
    /// cannot disagree, because there is one Rust definition and this asserts
    /// the wrapper is it.
    #[test]
    fn the_project_key_is_the_workload_identity_digest_and_not_a_second_one() {
        let expected = workload_role_identity_hash(
            WorkloadRoleFamily::App,
            WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: "acme",
                database: DATABASE,
            },
        )
        .expect("App takes a project identity");
        assert_eq!(project_key("acme", DATABASE), expected);
    }

    /// THE RUNTIME COMPUTES THIS DIGEST TOO, from `wamn-run-state`, because it
    /// must make sure that a resolved guest credential belongs to the project it is
    /// about to serve and the shipped runtime links no provisioner
    /// (`wamn-0h0g.22.6.7`).
    ///
    /// Both consumers call the same function. The live SQL agreement test
    /// compares this result with the independent PostgreSQL implementation.
    #[test]
    fn the_runtime_reachable_digest_is_the_same_one() {
        for (project_env_key, database) in [
            ("acme", DATABASE),
            ("t", "wamn-db-acme--prod--billing"),
            (&"t".repeat(64), DATABASE),
        ] {
            assert_eq!(
                project_key(project_env_key, database),
                wamn_identity_protocol::app_identity_hash(project_env_key, database),
                "project {project_env_key:?}: the provisioner and the runtime must agree"
            );
        }
    }

    /// Two project-environment databases must not derive one key for one
    /// project, or their guest logins collide on a single role name.
    #[test]
    fn the_same_project_env_key_derives_a_different_key_per_database() {
        assert_ne!(
            project_key("acme", DATABASE),
            project_key("acme", "wamn-db-acme--prod--billing"),
        );
    }

    /// A prefix carrying a regex metacharacter would turn the anchored pattern
    /// into a wildcard, and a wildcard here is a cross-project read.
    #[test]
    fn the_login_prefix_carries_no_regex_metacharacter() {
        let prefix = WorkloadRoleFamily::App.login_prefix();
        assert!(
            prefix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{prefix} would need escaping before it can go into a pattern"
        );
    }
}
