use std::error::Error;
use std::fmt;

/// Stable classification of package refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestErrorType {
    InvalidManifest,
    InvalidIdentity,
    UnknownRelation,
    UnknownColumn,
    InvalidModel,
    InvalidOperation,
    InvalidConnection,
    InvalidComponent,
    InvalidClientPackage,
    MissingAuthoredSql,
    UnexpectedAuthoredSql,
    SchemaQualifiedSql,
    DuplicatePath,
}

/// A deterministic package refusal with stable class and object context.
#[derive(Debug)]
pub struct ManifestError {
    type_: ManifestErrorType,
    context: Box<str>,
    object: Option<Box<str>>,
    path: Option<Box<str>>,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl ManifestError {
    /// Stable refusal class for callers that must not match display text.
    pub const fn error_type(&self) -> ManifestErrorType {
        self.type_
    }

    /// Package, model, operation, field, or path that caused the refusal.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Referenced manifest or IR object, when the refusal is object-targeted.
    pub fn object(&self) -> Option<&str> {
        self.object.as_deref()
    }

    /// Package-relative source or artifact path, when the refusal is path-targeted.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    #[doc(hidden)]
    pub fn new(type_: ManifestErrorType, context: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            context: context.into(),
            object: None,
            path: None,
            source: None,
        }
    }

    #[doc(hidden)]
    pub fn for_object(
        type_: ManifestErrorType,
        context: impl Into<Box<str>>,
        object: impl Into<Box<str>>,
    ) -> Self {
        Self {
            type_,
            context: context.into(),
            object: Some(object.into()),
            path: None,
            source: None,
        }
    }

    #[doc(hidden)]
    pub fn for_path(
        type_: ManifestErrorType,
        context: impl Into<Box<str>>,
        path: impl Into<Box<str>>,
    ) -> Self {
        Self {
            type_,
            context: context.into(),
            object: None,
            path: Some(path.into()),
            source: None,
        }
    }

    #[doc(hidden)]
    pub fn with_source(
        type_: ManifestErrorType,
        context: impl Into<Box<str>>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            context: context.into(),
            object: None,
            path: None,
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.type_, self.context)
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
