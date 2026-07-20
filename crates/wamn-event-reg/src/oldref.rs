//! Root-`old` detection over a registration [`condition`](crate::EventRegistration::condition).
//!
//! A "changed-to" condition reads the ROOT `old` image (`new.status !=
//! old.status`); such a registration needs the entity's **REPLICA IDENTITY
//! FULL** (the per-entity DDL knob l5i9.31) for the old image to be present in
//! the CDC event. Two consumers key on the SAME detection so they can never
//! diverge:
//!
//! - the **reconciler** (`wamn_migrate::reconcile_replica_identity`) derives
//!   which entities need FULL from the union of their registrations' old-image
//!   usage (+ delete-op subscription);
//! - the **materializer** (`wamn_materializer`) guards per event: a condition
//!   that reads root `old` cannot be evaluated when the delivered event carries
//!   no old image (RI DEFAULT) — old-absent is cannot-evaluate, never
//!   condition-false.
//!
//! This module is the ONE parser. It walks the compiled JMESPath AST; a
//! non-compiling expression is rejected at registration write ([`crate::validate`]),
//! so it can never reach a stored condition — [`condition_references_old`]
//! treats an uncompilable expression as "no old read" (there is nothing to flip
//! RI for), the safe default for a value validation already forbids.

use jmespath::ast::Ast;

/// Whether a JMESPath predicate reads the ROOT `old` image anywhere. Compiles
/// the expression and walks its AST; an uncompilable expression (rejected at
/// write) reads as `false`.
pub fn condition_references_old(expr: &str) -> bool {
    match jmespath::compile(expr) {
        Ok(compiled) => references_old(compiled.as_ast()),
        Err(_) => false,
    }
}

/// Whether the expression reads the ROOT `old` field anywhere. Root-relative:
/// `old.status` is a root read; `new.old` is a COLUMN named "old" and is fine.
/// The walk tracks whether each node still evaluates against the root context;
/// anything context-shifting (`Subexpr` rhs, projection rhs) clears the flag.
/// Deliberately conservative where JMESPath's evaluation context is dynamic
/// (`Expref` bodies inherit the flag): over-detecting only widens the RI-FULL
/// set / an alertable hold, whereas under-detecting would evaluate an absent
/// `old` as `null` — the exact "condition-false" corruption the contract forbids.
pub fn references_old(ast: &Ast) -> bool {
    at_root(ast, true)
}

fn at_root(ast: &Ast, root: bool) -> bool {
    match ast {
        Ast::Field { name, .. } => root && name == "old",
        // The rhs evaluates against the lhs's RESULT — no longer the root.
        Ast::Subexpr { lhs, rhs, .. } => at_root(lhs, root) || at_root(rhs, false),
        Ast::Projection { lhs, rhs, .. } => at_root(lhs, root) || at_root(rhs, false),
        // Both sides evaluate against the CURRENT context.
        Ast::Comparison { lhs, rhs, .. } | Ast::And { lhs, rhs, .. } | Ast::Or { lhs, rhs, .. } => {
            at_root(lhs, root) || at_root(rhs, root)
        }
        Ast::Condition {
            predicate, then, ..
        } => at_root(predicate, root) || at_root(then, root),
        Ast::Not { node, .. }
        | Ast::Flatten { node, .. }
        | Ast::ObjectValues { node, .. }
        | Ast::Expref { ast: node, .. } => at_root(node, root),
        Ast::Function { args, .. } => args.iter().any(|a| at_root(a, root)),
        Ast::MultiList { elements, .. } => elements.iter().any(|e| at_root(e, root)),
        // A MultiHash key is a literal string, not an expression.
        Ast::MultiHash { elements, .. } => elements.iter().any(|kv| at_root(&kv.value, root)),
        Ast::Identity { .. } | Ast::Index { .. } | Ast::Literal { .. } | Ast::Slice { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(expr: &str) -> bool {
        condition_references_old(expr)
    }

    #[test]
    fn root_old_reads_are_detected() {
        assert!(refs("old"));
        assert!(refs("old.status"));
        assert!(refs("old.status == 'open'"));
        assert!(refs("new.status != old.status")); // the changed-to shape
        assert!(refs("!old.archived"));
        assert!(refs("op == 'update' && old.qty"));
        assert!(refs("contains(old.tags, 'hot')"));
        assert!(refs("[new.id, old.id]"));
        assert!(refs("{was: old.status}"));
    }

    #[test]
    fn a_column_literally_named_old_is_not_a_root_read() {
        // `new.old` reads a COLUMN named "old" from the new image — root-safe.
        assert!(!refs("new.old"));
        assert!(!refs("new.old == 'x'"));
        // A string literal spelling "old" is data, not a read.
        assert!(!refs("new.status == 'old'"));
    }

    #[test]
    fn new_only_conditions_are_serviceable() {
        assert!(!refs("new.status == 'received'"));
        assert!(!refs("op == 'insert'"));
        assert!(!refs("new.qty && op != 'delete'"));
    }

    #[test]
    fn an_uncompilable_expression_reads_as_no_old() {
        // Validation rejects it at write; the RI derivation must not panic and
        // must not flip a table for a value that will never be stored.
        assert!(!refs("]not jmespath["));
    }
}
