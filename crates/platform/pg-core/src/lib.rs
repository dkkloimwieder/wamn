//! Guest-safe PostgreSQL vocabulary shared across bounded contexts.
//!
//! MVP outcome: event spine (causation depth = loop guard).
//!
//! A [`Sql`] is a parameterized statement fragment carried **with** its
//! positional-parameter arity (the count of distinct `$n` placeholders it binds).
//! The pure crates emit SQL as `String`; when a fragment produced in one crate is
//! composed into a statement in another — `wamn-run-state` emits the per-node
//! checkpoint and completion INSERT/UPDATEs and wraps them in a CTE
//! and appends a lease-renew tail — the consumer must number its own params AFTER
//! the head's. Hardcoding that offset (`$7`/`$8` on the assumption the head uses
//! `$1..$6`) is the SR11 bug: add one parameter upstream and the tail's TTL and
//! owner-guard silently shift onto the wrong binds, on a path every run executes.
//! The strings compose; their **contracts** did not — until the arity travels
//! with the text.
//!
//! This crate is the smallest sound home for that contract: a leaf with **no
//! dependencies** (guest-compilable, no DB/clock/wasm), that both the producer
//! (`wamn-run-state`) can depend on without a
//! cycle. Leaf builders that are never composed keep returning `String`.
//!
//! ```
//! use wamn_pg_core::Sql;
//!
//! // A head that binds $1..$6, composed with a tail that SHARES $1 and appends
//! // two NEW params. The tail numbers them against the head's arity, so they land
//! // at $7/$8 — and if the head ever grows to $1..$7, `param` yields $8/$9 with no
//! // edit at the call site.
//! let head = Sql::new("INSERT ... $1 ... $6", 6);
//! let composed = format!(
//!     "WITH h AS ({head}) UPDATE t SET ttl = ${ttl} WHERE id = $1 AND owner = ${owner}",
//!     head = head.text(),
//!     ttl = head.param(1),
//!     owner = head.param(2),
//! );
//! assert!(composed.contains("ttl = $7"));
//! assert!(composed.contains("owner = $8"));
//! ```
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: a prior run-queue batch claim
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

/// A parameterized SQL fragment carried with its positional-parameter arity — the
/// count of distinct `$1..$n` placeholders it binds. Construct one where the SQL
/// is authored (so the text and its arity change together), and a downstream
/// composer numbers its own tail params with [`Sql::param`] instead of hardcoding
/// an offset that a new upstream param would silently break (SR11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sql {
    text: String,
    arity: u16,
}

impl Sql {
    /// A fragment binding `$1..=arity`. `arity` is the author's declaration of how
    /// many params the text uses; keep it beside the text so the two cannot drift
    /// (the producing crate asserts arity == the highest `$n` in `text`).
    pub fn new(text: impl Into<String>, arity: u16) -> Sql {
        Sql {
            text: text.into(),
            arity,
        }
    }

    /// The fragment text (its placeholders are `$1..=arity`).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many positional params the fragment binds (`$1..=arity`).
    pub fn arity(&self) -> u16 {
        self.arity
    }

    /// The 1-based placeholder index of the `nth` parameter appended AFTER this
    /// fragment's own: `param(1) == arity + 1`. A composing site writes its tail's
    /// new params as `${head.param(1)}`, `${head.param(2)}`, … so growing the head
    /// by one param shifts them automatically rather than misbinding. Params the
    /// tail SHARES with the head (e.g. a run id at `$1`) keep their original index
    /// and are written literally; only the tail's NEW params use `param`.
    pub fn param(&self, nth: u16) -> u16 {
        self.arity + nth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_offsets_against_arity() {
        let head = Sql::new("x", 6);
        assert_eq!(head.arity(), 6);
        assert_eq!(head.param(1), 7);
        assert_eq!(head.param(2), 8);
    }

    #[test]
    fn a_larger_head_shifts_the_appended_params() {
        // The SR11 property: the SAME composing code numbers correctly for any head
        // arity, so an upstream param addition can never silently misbind the tail.
        assert_eq!(Sql::new("x", 7).param(1), 8);
        assert_eq!(Sql::new("x", 7).param(2), 9);
        assert_eq!(Sql::new("x", 2).param(1), 3);
    }

    #[test]
    fn text_and_arity_round_trip() {
        let s = Sql::new("SELECT $1, $2", 2);
        assert_eq!(s.text(), "SELECT $1, $2");
        assert_eq!(s.arity(), 2);
    }

    #[test]
    fn quoting_and_identifier_validation_cover_adversarial_names() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_ident(""), "\"\"");
        assert_eq!(quote_literal("a'b"), "'a''b'");
        assert!(Identifier::new("").is_err());
        assert!(Identifier::new("nul\0byte").is_err());
        assert!(Identifier::new("é".repeat(31)).is_ok());
        assert!(Identifier::new("é".repeat(32)).is_err());
        assert_eq!(
            QualifiedName::new(
                Identifier::new("odd schema").unwrap(),
                Identifier::new("t\"able").unwrap()
            )
            .quoted(),
            "\"odd schema\".\"t\"\"able\""
        );
    }
}
use std::fmt;

/// Schema hosting the per-database authority derivations (`wamn-0h0g.22.6`).
pub const AUTHORITY_SCHEMA: &str = "wamn_authority";

/// The schema-qualified derivation from a row's `tenant_id` to its tenant key.
///
/// Lives in this leaf so the provisioner that CREATES it and the schema
/// compiler that EMITS CALLS TO IT name one object. An unqualified call would
/// resolve through the caller's `search_path`, which is the settable hijack
/// `wamn-0h0g.22.6`'s option (c) exists to remove — so this constant carries
/// the qualification and every use site takes it whole.
pub const TENANT_KEY_FUNCTION: &str = "wamn_authority.tenant_key";

/// The schema-qualified derivation from `current_user` to the connected role's
/// tenant key (`wamn-0h0g.22.6.5`).
pub const CURRENT_TENANT_KEY_FUNCTION: &str = "wamn_authority.current_tenant_key";

/// Quote a PostgreSQL identifier without changing its contents.
pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Quote a PostgreSQL string literal without changing its contents.
pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// A validated PostgreSQL identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identifier(String);

/// Why an identifier cannot be used as a PostgreSQL name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidIdentifier {
    reason: &'static str,
}

impl InvalidIdentifier {
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for InvalidIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for InvalidIdentifier {}

impl Identifier {
    /// Validate a name before it crosses into generated SQL.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidIdentifier> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidIdentifier {
                reason: "identifier is empty",
            });
        }
        if value.as_bytes().contains(&0) {
            return Err(InvalidIdentifier {
                reason: "identifier contains NUL",
            });
        }
        if value.len() > 63 {
            return Err(InvalidIdentifier {
                reason: "identifier exceeds PostgreSQL's 63-byte limit",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn quoted(&self) -> String {
        quote_ident(&self.0)
    }
}

/// A schema-qualified PostgreSQL name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedName {
    schema: Identifier,
    name: Identifier,
}

impl QualifiedName {
    pub fn new(schema: Identifier, name: Identifier) -> Self {
        Self { schema, name }
    }

    pub fn quoted(&self) -> String {
        format!("{}.{}", self.schema.quoted(), self.name.quoted())
    }
}
