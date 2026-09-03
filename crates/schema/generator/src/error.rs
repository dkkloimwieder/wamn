use std::error::Error;
use std::fmt;

/// Stable classification of generation refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateErrorKind {
    InvalidManifest,
    InvalidIdentity,
    UnknownRelation,
    UnknownColumn,
    InvalidModel,
    InvalidOperation,
    InvalidConnection,
    InvalidComponent,
    InvalidDistribution,
    MissingAuthoredSql,
    UnexpectedAuthoredSql,
    SchemaQualifiedSql,
    DuplicatePath,
}

/// A deterministic generation refusal with stable class and object context.
#[derive(Debug)]
pub struct GenerateError {
    kind: GenerateErrorKind,
    context: Box<str>,
    object: Option<Box<str>>,
    path: Option<Box<str>>,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl GenerateError {
    /// Stable refusal class for callers that must not match display text.
    pub const fn kind(&self) -> GenerateErrorKind {
        self.kind
    }

    /// Package, model, operation, field, or path that caused the refusal.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Referenced manifest or IR object, when the refusal is object-scoped.
    pub fn object(&self) -> Option<&str> {
        self.object.as_deref()
    }

    /// Package-relative source or artifact path, when the refusal is path-scoped.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub(crate) fn new(kind: GenerateErrorKind, context: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            context: context.into(),
            object: None,
            path: None,
            source: None,
        }
    }

    pub(crate) fn for_object(
        kind: GenerateErrorKind,
        context: impl Into<Box<str>>,
        object: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            object: Some(object.into()),
            path: None,
            source: None,
        }
    }

    pub(crate) fn for_path(
        kind: GenerateErrorKind,
        context: impl Into<Box<str>>,
        path: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            object: None,
            path: Some(path.into()),
            source: None,
        }
    }

    pub(crate) fn with_source(
        kind: GenerateErrorKind,
        context: impl Into<Box<str>>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            context: context.into(),
            object: None,
            path: None,
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for GenerateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.context)
    }
}

impl Error for GenerateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
