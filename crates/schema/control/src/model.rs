//! Effect-shell statement values shared by pure reconcilers.

/// A positional bind value for an [`SqlStatement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    NullableText(Option<String>),
    Int(i32),
    NullableInt(Option<i32>),
    Bool(bool),
}

/// One SQL statement and its positional bind values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlStatement {
    pub summary: String,
    pub sql: String,
    pub params: Vec<Value>,
}
