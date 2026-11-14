//! Input refusals shared by generated typed exports.

/// A typed input that fails its declared envelope constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputError {
    context: &'static str,
}
impl InputError {
    /// Describe the violated input constraint.
    pub const fn new(context: &'static str) -> Self {
        Self { context }
    }
    /// The constraint that refused the input.
    pub const fn context(&self) -> &'static str {
        self.context
    }
}
impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.context)
    }
}
impl std::error::Error for InputError {}
