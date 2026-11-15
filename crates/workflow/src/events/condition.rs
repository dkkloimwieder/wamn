//! Compile and evaluate event workflow conditions.

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
    /// (workflow authoring) should have rejected it; holding here is the backstop.
    Invalid(String),
}

/// Compile a registration condition. Only invalid syntax is unserviceable now —
/// a root-`old` predicate compiles and is guarded per event (l5i9.31).
pub fn compile_condition(expr: &str) -> Result<CompiledCondition, ConditionOutcome> {
    let compiled = jmespath::compile(expr).map_err(|e| ConditionOutcome::Invalid(e.to_string()))?;
    let references_old = super::oldref::references_old(compiled.as_ast());
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
