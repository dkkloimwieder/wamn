//! Detect root old-image reads in event workflow predicates.

use jmespath::ast::Ast;

/// Whether the expression reads the ROOT `old` field anywhere. Root-relative:
/// `old.status` is a root read; `new.old` is a COLUMN named "old" and is fine.
/// The walk tracks whether each node still evaluates against the root context;
/// anything context-shifting (`Subexpr` rhs, projection rhs) clears the flag.
/// Deliberately conservative where JMESPath's evaluation context is dynamic
/// (`Expref` bodies inherit the flag): over-detecting only widens the RI-FULL
/// set / an alertable hold, whereas under-detecting would evaluate an absent
/// `old` as `null` — the exact "condition-false" corruption the contract forbids.
pub(super) fn references_old(ast: &Ast) -> bool {
    at_root(ast, true)
}

fn at_root(ast: &Ast, root: bool) -> bool {
    match ast {
        Ast::Field { name, .. } => root && name == "old",
        // The rhs evaluates against the lhs's RESULT — no longer the root.
        Ast::Subexpr { lhs, rhs, .. } | Ast::Projection { lhs, rhs, .. } => {
            at_root(lhs, root) || at_root(rhs, false)
        }
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
