//! Condition compilation + evaluation — a JMESPath **predicate** over the
//! event context `{"op", "old", "new"}` ([`crate::event_context`]).
//!
//! Under the per-entity REPLICA IDENTITY FULL knob (l5i9.31) a condition that
//! reads the ROOT `old` image is **serviceable** — it compiles and evaluates —
//! but the old image is present only when the entity runs FULL and the op
//! carries one. [`compile_condition`] records whether the predicate reads root
//! `old` ([`CompiledCondition::references_old`]) so the caller can guard: when a
//! needed old image is ABSENT the event is CANNOT-EVALUATE (an alertable
//! refusal, `crate::decide`), never condition-false. The single root-`old`
//! detector lives in `wamn_event_reg` — this crate reuses it so the reconciler
//! (which derives the FULL set) and the materializer can never diverge.

/// A compiled, serviceable condition (syntactically valid), plus whether it
/// reads the ROOT `old` image (the per-event old-absent guard keys on this).
#[derive(Debug)]
pub struct CompiledCondition {
    expr: jmespath::Expression<'static>,
    references_old: bool,
}

/// Why a condition cannot be compiled into a serviceable form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionOutcome {
    /// The expression does not parse. Registration-time validation
    /// (wamn-event-reg) should have rejected it; holding here is the backstop.
    Invalid(String),
}

/// Compile a registration condition. Only invalid syntax is unserviceable now —
/// a root-`old` predicate compiles and is guarded per event (l5i9.31).
pub fn compile_condition(expr: &str) -> Result<CompiledCondition, ConditionOutcome> {
    let compiled = jmespath::compile(expr).map_err(|e| ConditionOutcome::Invalid(e.to_string()))?;
    let references_old = wamn_event_reg::references_old(compiled.as_ast());
    Ok(CompiledCondition {
        expr: compiled,
        references_old,
    })
}

impl CompiledCondition {
    /// Whether this predicate reads the ROOT `old` image. When `true`, the
    /// caller must refuse (cannot-evaluate) an event that carries no old image
    /// rather than evaluate `old` as `null`.
    pub fn references_old(&self) -> bool {
        self.references_old
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compile_records_old_reference_and_rejects_bad_syntax() {
        // A root-`old` predicate is now SERVICEABLE (no longer blocked) and is
        // flagged as reading the old image.
        let old = compile_condition("new.status != old.status").expect("old-ref compiles");
        assert!(old.references_old(), "changed-to predicate reads old");

        let new_only = compile_condition("new.status == 'received'").expect("new-only compiles");
        assert!(!new_only.references_old());

        // A column literally named `old` on the NEW image is not a root read.
        assert!(!compile_condition("new.old == 'x'").unwrap().references_old());

        assert!(matches!(
            compile_condition("]not jmespath["),
            Err(ConditionOutcome::Invalid(_))
        ));
    }

    #[test]
    fn matches_uses_jmespath_truthiness_over_the_event_context() {
        let cond = compile_condition("new.status == 'received'").unwrap();
        let hit = json!({"op": "insert", "old": null, "new": {"status": "received"}});
        let miss = json!({"op": "insert", "old": null, "new": {"status": "draft"}});
        assert!(cond.matches(&hit).unwrap());
        assert!(!cond.matches(&miss).unwrap());

        // A missing field is null → falsey, for a NEW-image condition.
        let sparse = json!({"op": "insert", "old": null, "new": {}});
        assert!(!cond.matches(&sparse).unwrap());
    }

    #[test]
    fn an_old_condition_evaluates_when_the_old_image_is_present() {
        // With the old image present (RI FULL), a changed-to predicate is a
        // normal boolean — both outcomes reachable.
        let cond = compile_condition("new.status != old.status").unwrap();
        let changed = json!({"op": "update", "old": {"status": "draft"}, "new": {"status": "shipped"}});
        let unchanged = json!({"op": "update", "old": {"status": "shipped"}, "new": {"status": "shipped"}});
        assert!(cond.matches(&changed).unwrap());
        assert!(!cond.matches(&unchanged).unwrap());
    }
}
