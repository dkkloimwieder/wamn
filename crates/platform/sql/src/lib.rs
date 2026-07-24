//! # wamn-sql (SR11) — the SQL composition primitive
//!
//! A [`Sql`] is a parameterized statement fragment carried **with** its
//! positional-parameter arity (the count of distinct `$n` placeholders it binds).
//! The pure crates emit SQL as `String`; when a fragment produced in one crate is
//! composed into a statement in another — `wamn-run-store` emits the per-node
//! checkpoint and completion INSERT/UPDATEs, `wamn-run-queue` wraps them in a CTE
//! and appends a lease-renew tail — the consumer must number its own params AFTER
//! the head's. Hardcoding that offset (`$7`/`$8` on the assumption the head uses
//! `$1..$6`) is the SR11 bug: add one parameter upstream and the tail's TTL and
//! owner-guard silently shift onto the wrong binds, on a path every run executes.
//! The strings compose; their **contracts** did not — until the arity travels
//! with the text.
//!
//! This crate is the smallest sound home for that contract: a leaf with **no
//! dependencies** (guest-compilable, no DB/clock/wasm), that both the producer
//! (`wamn-run-store`) and the consumer (`wamn-run-queue`) depend on without a
//! cycle. Leaf builders that are never composed keep returning `String`.
//!
//! ```
//! use wamn_sql::Sql;
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
//! correctly here and still misbehave live: `wamn-run-queue`'s `claim_batch_sql`
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
}
