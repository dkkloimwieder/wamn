//! The pure derivations guest RLS uses to reach a tenant from `current_user`.
//!
//! # Why functions and not a mapping table (`wamn-0h0g.22.6`, option (c))
//!
//! Guest tenant authority must come from `current_user`, because a GUC claim is
//! settable by whoever holds the session. The obvious replacement — a table
//! mapping login to tenant — is banned, and the ban is precise: it targets
//! MUTABLE, SETTABLE STATE between identity and authority. A fixed derivation
//! with no writer and no state is computation, not indirection.
//!
//! # The two halves of one predicate
//!
//! [`tenant_key`] derives a key FROM A ROW; [`current_tenant_key_pattern`]
//! backs the function that derives the key OF THE CONNECTED ROLE. The governed
//! predicate is the equality of the two:
//!
//! ```sql
//! wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()
//! ```
//!
//! The left side is the BARE call the expression index is built on, which is
//! the only form PostgreSQL will match that index against — so the right side
//! has to be a scalar, and every shape that wraps the indexed expression is
//! refuted by the same ruling that demands the index.
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
//! time would make [`tenant_key`]'s SQL twin unusable in an expression index —
//! destroying the sargability that option (c) depends on. It also cannot be
//! dropped from the preimage: without it, one tenant in two project-environment
//! databases derives the same key and collides on a single role name.

use wamn_pg_core::{quote_ident, quote_literal};
use wamn_run_state::CredentialGeneration;

use crate::workload_role::{
    SCOPE_HASH_HEX_LEN, WorkloadRoleFamily, WorkloadRoleScope, workload_role_scope_hash,
};
use crate::{APP_ROLE, DB_OWNER_ROLE};

/// Schema hosting authority derivations inside a project-environment database.
///
/// Same name as the control plane's `wamn_authority`, and deliberately a
/// SEPARATE OBJECT in a separate database — the two-plane residency pattern.
///
/// Re-exported, not redefined: the schema compiler EMITS CALLS to these
/// functions and this crate CREATES them, so the names live in the leaf both
/// depend on rather than once per crate.
pub use wamn_pg_core::{
    AUTHORITY_SCHEMA as TENANT_KEY_SCHEMA, CURRENT_TENANT_KEY_FUNCTION, TENANT_KEY_FUNCTION,
};

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

/// The anchored pattern that recovers a tenant key from a guest login name.
///
/// Every part is read from the SAME constants [`workload_generation_role`]
/// composes a role name from — the family's generation prefix, the digest
/// width, and the closed generation suffixes — so the naming decision on
/// `wamn-0h0g.22.6.4` cannot move the mint without moving this pattern with it.
///
/// Anchored at both ends: an unanchored pattern would match a crafted role name
/// that merely CONTAINS a victim's key.
///
/// [`workload_generation_role`]: crate::workload_role::workload_generation_role
pub fn current_tenant_key_pattern() -> String {
    let generations: String = [CredentialGeneration::A, CredentialGeneration::B]
        .iter()
        .map(|generation| generation.as_str())
        .collect();
    format!(
        "^{prefix}_([0-9a-f]{{{width}}})_[{generations}]$",
        prefix = WorkloadRoleFamily::App.generation_prefix(),
        width = SCOPE_HASH_HEX_LEN,
    )
}

/// Owner the bootstrap rendering gives the schema and both functions.
///
/// `CURRENT_USER`, not the named platform role. The static DDL files that carry
/// this bootstrap already declare `AUTHORIZATION postgres` or `AUTHORIZATION
/// CURRENT_USER`, and naming a role that may not exist would add a new
/// precondition to every applier — seven live gates plus the production path in
/// `services/ctl`. The applier IS the platform role in every real deployment,
/// so `CURRENT_USER` satisfies the ownership rider without inventing a
/// requirement.
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
/// the proof they agree — a live test applies both and compares the digests
/// `pg_get_functiondef` reports.
///
/// `SET search_path = pg_catalog` pins every builtin these bodies call, so
/// neither derivation can be redirected by a caller's search path.
fn derivations_template(database_literal: &str, database_octets: &str, owner: &str) -> String {
    let domain = WorkloadRoleFamily::App.scope_domain();
    let domain_text = std::str::from_utf8(domain).expect("the scope domain is ASCII");
    format!(
        "CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION {owner};\n\
         REVOKE ALL ON SCHEMA {schema} FROM PUBLIC;\n\
         GRANT USAGE ON SCHEMA {schema} TO {app};\n\
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
         || int8send({database_octets}::bigint) || convert_to({database_literal}, 'UTF8')\n       \
         ), 'hex'), 1, {hex_len})\n\
         $$;\n\
         ALTER FUNCTION {schema}.tenant_key(text) OWNER TO {owner};\n\
         REVOKE ALL ON FUNCTION {schema}.tenant_key(text) FROM PUBLIC;\n\
         GRANT EXECUTE ON FUNCTION {schema}.tenant_key(text) TO {app};\n\
         CREATE OR REPLACE FUNCTION {schema}.current_tenant_key()\n\
         RETURNS text\n\
         LANGUAGE sql\n\
         STABLE\n\
         PARALLEL SAFE\n\
         SET search_path = pg_catalog\n\
         AS $$\n    \
         SELECT substring(current_user::text from {pattern_lit})\n\
         $$;\n\
         ALTER FUNCTION {schema}.current_tenant_key() OWNER TO {owner};\n\
         REVOKE ALL ON FUNCTION {schema}.current_tenant_key() FROM PUBLIC;\n\
         GRANT EXECUTE ON FUNCTION {schema}.current_tenant_key() TO {app};",
        schema = quote_ident(TENANT_KEY_SCHEMA),
        app = quote_ident(APP_ROLE),
        domain_len = domain.len(),
        domain_lit = quote_literal(domain_text),
        tenant_tag_len = "tenant".len(),
        database_tag_len = "database".len(),
        hex_len = SCOPE_HASH_HEX_LEN,
        pattern_lit = quote_literal(&current_tenant_key_pattern()),
    )
}

/// The DDL creating both authority derivations for a KNOWN database.
///
/// Idempotent by construction (`CREATE OR REPLACE`, `IF NOT EXISTS`), which is
/// what lets it converge on `wamn-0h0g.11.49`'s path for existing databases
/// while `catalog-schema.sql` carries fresh installs. Idempotence is not the
/// proof, though: the converge arm asserts POST-STATE — the definition digest,
/// the volatility and parallel-safety flags read from `pg_proc`, and exact
/// grants.
///
/// Both functions land in ONE builder because both must reach both appliers;
/// two builders would be two places to remember and one place to forget.
pub fn authority_derivations_sql(database: &str) -> String {
    derivations_template(
        &quote_literal(database),
        &database.len().to_string(),
        &quote_ident(DB_OWNER_ROLE),
    )
}

/// The same DDL for a database whose name is NOT known at authoring time.
///
/// `catalog-schema.sql` and `app-schema.sql` are static files applied to
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
    use super::{
        BOOTSTRAP_OWNER, CURRENT_TENANT_KEY_FUNCTION, DATABASE_LITERAL_PLACEHOLDER,
        DATABASE_OCTETS_PLACEHOLDER, TENANT_KEY_FUNCTION, authority_derivations_bootstrap_sql,
        authority_derivations_sql, current_tenant_key_pattern, tenant_key,
    };
    use crate::workload_role::{
        SCOPE_HASH_HEX_LEN, WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
        workload_role_scope_hash,
    };
    use wamn_run_state::CredentialGeneration;

    const DATABASE: &str = "wamn-db-acme--billing--dev";

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
                database: DATABASE,
            },
        )
        .expect("App takes a tenant scope");
        assert_eq!(tenant_key("acme", DATABASE), expected);
    }

    /// Two project-environment databases must not derive one key for one
    /// tenant, or their guest logins collide on a single role name.
    #[test]
    fn the_same_tenant_derives_a_different_key_per_database() {
        assert_ne!(
            tenant_key("acme", DATABASE),
            tenant_key("acme", "wamn-db-acme--billing--prod"),
        );
    }

    /// The session side assumes a role name shaped exactly as the mint composes
    /// it. This asserts the shape against a role the mint ACTUALLY produced, so
    /// a change to either half shows up here rather than as a silent refusal of
    /// every guest read.
    #[test]
    fn the_minted_role_carries_exactly_the_key_the_pattern_captures() {
        let head = format!("{}_", WorkloadRoleFamily::App.generation_prefix());
        let key = tenant_key("acme", DATABASE);
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            let role = workload_generation_role(
                WorkloadRoleFamily::App,
                WorkloadRoleScope::Tenant {
                    tenant: "acme",
                    database: DATABASE,
                },
                generation,
            )
            .expect("App takes a tenant scope");
            let rest = role
                .strip_prefix(&head)
                .expect("the mint composes from the family generation prefix");
            let (captured, suffix) = rest.split_at(SCOPE_HASH_HEX_LEN);
            assert_eq!(captured, key, "the role carries the tenant key verbatim");
            assert_eq!(suffix, format!("_{}", generation.as_str()));
        }
        assert!(
            current_tenant_key_pattern().starts_with(&format!("^{head}(")),
            "the pattern's literal head is the mint's prefix"
        );
    }

    /// A prefix carrying a regex metacharacter would turn the anchored pattern
    /// into a wildcard, and a wildcard here is a cross-tenant read.
    #[test]
    fn the_generation_prefix_carries_no_regex_metacharacter() {
        let prefix = WorkloadRoleFamily::App.generation_prefix();
        assert!(
            prefix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{prefix} would need escaping before it can go into a pattern"
        );
    }

    /// GENERATED SQL FROM A RUST BUILDER, so the string is pinned: it is the
    /// only thing that catches the builder moving, and this builder's output is
    /// a security boundary.
    #[test]
    fn the_emitted_ddl_is_frozen() {
        let sql = authority_derivations_sql(DATABASE);
        assert_eq!(
            sql,
            "CREATE SCHEMA IF NOT EXISTS \"wamn_authority\" AUTHORIZATION \"wamn_db_owner\";\n\
             REVOKE ALL ON SCHEMA \"wamn_authority\" FROM PUBLIC;\n\
             GRANT USAGE ON SCHEMA \"wamn_authority\" TO \"wamn_app\";\n\
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
             GRANT EXECUTE ON FUNCTION \"wamn_authority\".tenant_key(text) TO \"wamn_app\";\n\
             CREATE OR REPLACE FUNCTION \"wamn_authority\".current_tenant_key()\n\
             RETURNS text\n\
             LANGUAGE sql\n\
             STABLE\n\
             PARALLEL SAFE\n\
             SET search_path = pg_catalog\n\
             AS $$\n    \
             SELECT substring(current_user::text from '^wamn_app_([0-9a-f]{40})_[ab]$')\n\
             $$;\n\
             ALTER FUNCTION \"wamn_authority\".current_tenant_key() OWNER TO \"wamn_db_owner\";\n\
             REVOKE ALL ON FUNCTION \"wamn_authority\".current_tenant_key() FROM PUBLIC;\n\
             GRANT EXECUTE ON FUNCTION \"wamn_authority\".current_tenant_key() TO \"wamn_app\";"
        );
    }

    /// The qualified names are the contract every predicate must use.
    #[test]
    fn the_function_names_are_schema_qualified() {
        assert_eq!(TENANT_KEY_FUNCTION, "wamn_authority.tenant_key");
        assert_eq!(
            CURRENT_TENANT_KEY_FUNCTION,
            "wamn_authority.current_tenant_key"
        );
        for name in [TENANT_KEY_FUNCTION, CURRENT_TENANT_KEY_FUNCTION] {
            assert!(
                name.contains('.'),
                "an unqualified name would resolve through the caller's search_path"
            );
        }
    }

    /// The bootstrap rendering must differ from the literal one in EXACTLY the
    /// two database fragments and nothing else. Substituting the placeholders
    /// by hand reproduces the literal rendering byte for byte — so anything
    /// else that drifted between the two would surface here rather than as a
    /// key that silently disagrees on one deployment.
    #[test]
    fn the_bootstrap_substitutes_the_database_and_changes_nothing_else() {
        let bootstrap = authority_derivations_bootstrap_sql();
        let substituted = bootstrap
            .replace(DATABASE_LITERAL_PLACEHOLDER, "'wamn-db-acme--billing--dev'")
            .replace(DATABASE_OCTETS_PLACEHOLDER, "26")
            .replace(BOOTSTRAP_OWNER, "\"wamn_db_owner\"");
        assert!(
            substituted.contains(&authority_derivations_sql(DATABASE)),
            "the bootstrap must carry the literal rendering verbatim once its \
             two database fragments and its owner are filled in"
        );

        // The PAIRING, not just the presence: a placeholder that the template
        // carries but the replace argument misspells survives into the emitted
        // DDL, and the installed function then hashes the placeholder text
        // instead of the database name. Only the live gate saw that until this
        // assertion existed, and the ordinary sweep runs no live gate.
        assert!(
            bootstrap.contains(&format!(
                "'{DATABASE_LITERAL_PLACEHOLDER}', quote_literal(current_database())"
            )),
            "the database literal must be substituted THROUGH quote_literal"
        );
        assert!(
            bootstrap.contains(&format!(
                "'{DATABASE_OCTETS_PLACEHOLDER}', octet_length(convert_to(current_database(), 'UTF8'))"
            )),
            "the length must be counted in OCTETS, matching Rust's str::len"
        );
    }

    /// A placeholder that survived into the emitted DDL would install a
    /// function whose preimage contains the placeholder text — a key that
    /// matches no minted role name, and therefore every guest read refusing.
    #[test]
    fn the_literal_rendering_carries_no_placeholder() {
        let sql = authority_derivations_sql(DATABASE);
        assert!(!sql.contains(DATABASE_LITERAL_PLACEHOLDER));
        assert!(!sql.contains(DATABASE_OCTETS_PLACEHOLDER));
    }
}
