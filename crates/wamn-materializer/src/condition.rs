//! Condition compilation + evaluation — a JMESPath **predicate** over the
//! event context `{"op", "old", "new"}` ([`crate::event_context`]), with the
//! REPLICA IDENTITY DEFAULT contract enforced structurally: a condition that
//! reads the ROOT `old` image is not evaluatable in v1 (old images are absent
//! under DEFAULT, and old-absent is CANNOT-EVALUATE, never condition-false),
//! so [`compile_condition`] rejects it and the caller HOLDS the registration
//! until the per-entity FULL knob (l5i9.31) ships.

use jmespath::ast::Ast;

/// A compiled, serviceable condition (root-`old`-free, syntactically valid).
#[derive(Debug)]
pub struct CompiledCondition {
    expr: jmespath::Expression<'static>,
}

/// Why a condition cannot be compiled into a serviceable form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionOutcome {
    /// The expression does not parse. Registration-time validation
    /// (wamn-event-reg) should have rejected it; holding here is the backstop.
    Invalid(String),
    /// The expression references the ROOT `old` image — blocked until the
    /// entity runs REPLICA IDENTITY FULL (l5i9.31, applied at/before
    /// registration). Old-absent = cannot-evaluate, never condition-false.
    OldValueBlocked,
}

/// Compile a registration condition, refusing root-`old` references (v1).
pub fn compile_condition(expr: &str) -> Result<CompiledCondition, ConditionOutcome> {
    let compiled = jmespath::compile(expr).map_err(|e| ConditionOutcome::Invalid(e.to_string()))?;
    if references_old(compiled.as_ast()) {
        return Err(ConditionOutcome::OldValueBlocked);
    }
    Ok(CompiledCondition { expr: compiled })
}

impl CompiledCondition {
    /// Evaluate against the event context; JMESPath truthiness (`false`,
    /// `null`, empty string/array/object are falsey). An evaluation error is a
    /// distinct outcome the caller refuses on — never silently condition-false.
    pub fn matches(&self, context: &serde_json::Value) -> Result<bool, String> {
        self.expr
            .search(context)
            .map(|v| v.is_truthy())
            .map_err(|e| e.to_string())
    }
}

/// Whether the expression reads the ROOT `old` field anywhere. Root-relative:
/// `old.status` is a root read; `new.old` is a COLUMN named "old" and is fine.
/// The walk tracks whether each node still evaluates against the root context;
/// anything context-shifting (`Subexpr` rhs, projection rhs) clears the flag.
/// Deliberately conservative where JMESPath's evaluation context is dynamic
/// (`Expref` bodies inherit the flag): over-blocking is an alertable hold,
/// under-blocking would evaluate an absent `old` as `null` — the exact
/// "condition-false" corruption the contract forbids.
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
    use serde_json::json;

    fn refs(expr: &str) -> bool {
        references_old(jmespath::compile(expr).expect("compiles").as_ast())
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
    fn compile_blocks_old_and_rejects_bad_syntax() {
        assert!(matches!(
            compile_condition("old.status == 'x'"),
            Err(ConditionOutcome::OldValueBlocked)
        ));
        assert!(matches!(
            compile_condition("]not jmespath["),
            Err(ConditionOutcome::Invalid(_))
        ));
        assert!(compile_condition("new.status == 'received'").is_ok());
    }

    #[test]
    fn matches_uses_jmespath_truthiness_over_the_event_context() {
        let cond = compile_condition("new.status == 'received'").unwrap();
        let hit = json!({"op": "insert", "old": null, "new": {"status": "received"}});
        let miss = json!({"op": "insert", "old": null, "new": {"status": "draft"}});
        assert!(cond.matches(&hit).unwrap());
        assert!(!cond.matches(&miss).unwrap());

        // A missing field is null → falsey, for a NEW-image condition (the
        // old-absent hazard is excluded structurally by compile_condition).
        let sparse = json!({"op": "insert", "old": null, "new": {}});
        assert!(!cond.matches(&sparse).unwrap());
    }
}
