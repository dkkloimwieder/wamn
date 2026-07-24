use std::fmt;

/// A catalog-derived entity operation that cannot be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntityAccessError {
    UnknownEntity(String),
    UnknownField {
        entity: String,
        field: String,
    },
    UnknownRelation {
        entity: String,
        relation: String,
    },
    UnsupportedExpansion {
        entity: String,
        relation: String,
        cardinality: &'static str,
    },
    UnservableRelation {
        entity: String,
        relation: String,
    },
    InvalidValue {
        field: String,
        message: String,
    },
    InvalidRequest(String),
}

impl EntityAccessError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownEntity(_) => "unknown-entity",
            Self::UnknownField { .. } => "unknown-field",
            Self::UnknownRelation { .. } => "unknown-relation",
            Self::UnsupportedExpansion { .. } => "unsupported-expansion",
            Self::UnservableRelation { .. } => "unservable-relation",
            Self::InvalidValue { .. } => "invalid-value",
            Self::InvalidRequest(_) => "invalid-request",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::UnknownEntity(entity) => format!("no such entity: {entity}"),
            Self::UnknownField { entity, field } => format!("no such field on {entity}: {field}"),
            Self::UnknownRelation { entity, relation } => {
                format!("no such relation on {entity}: {relation}")
            }
            Self::UnsupportedExpansion {
                entity,
                relation,
                cardinality,
            } => format!(
                "expansion of {cardinality} relation {relation} on {entity} is not supported"
            ),
            Self::UnservableRelation { entity, relation } => {
                format!("relation {relation} on {entity} is not expandable (no foreign-key field)")
            }
            Self::InvalidValue { field, message } => {
                format!("invalid value for {field}: {message}")
            }
            Self::InvalidRequest(message) => message.clone(),
        }
    }
}

impl fmt::Display for EntityAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code(), self.message())
    }
}

impl std::error::Error for EntityAccessError {}
