//! Project service and PAT errors.

use std::{error::Error, fmt};

/// Classification of a project service error at the control boundary.
#[derive(Debug, Clone, Copy)]
pub enum ServiceErrorType {
    InvalidInput(&'static str),
    NotFound(&'static str),
    DomainUnset,
    Database,
    Entropy,
}

/// A failed service operation with its original error and context.
#[derive(Debug)]
pub struct ServiceError {
    pub type_: ServiceErrorType,
    context: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ServiceError {
    pub fn refusal(type_: ServiceErrorType, context: &'static str) -> Self {
        Self {
            type_,
            context,
            source: None,
        }
    }

    pub fn database(context: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self::with_source(ServiceErrorType::Database, context, source)
    }

    pub fn with_source(
        type_: ServiceErrorType,
        context: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            context,
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl Error for ServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}
