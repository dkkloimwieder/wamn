//! Stable identity failure classes.

use std::fmt;

/// Stable classes of identity-core failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityErrorType {
    /// Caller input violated a local invariant.
    InvalidInput,
    /// A principal with the same type and subject already exists.
    Conflict,
    /// A referenced principal or project does not exist.
    NotFound,
    /// Stored identity data violated the crate's model.
    CorruptData,
    /// PostgreSQL rejected or failed an operation.
    Database,
    /// The operating system could not supply secure random bytes.
    Entropy,
    /// The configured envelope backend refused signing-key encryption or decryption.
    Encryption,
}

/// Why a write refused the caller's input, for a caller that answers each
/// refusal with its own code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRefusal {
    /// The input member of this name is not valid.
    Invalid(&'static str),
    /// The project is not a project of the org.
    ProjectNotFound,
    /// The project is a project of the org already.
    ProjectExists,
    /// The environment is not an environment of the org.
    EnvironmentNotFound,
    /// The principal is not an active member of the org.
    UserNotActive,
    /// The principal is not a member of the org, or not a user.
    UserNotFound,
    /// The user belongs to another org.
    UserInAnotherOrg,
    /// `org-admin` or `project-admin` covers the grant, so it stays.
    AdminCovered,
}

/// Canonical identity-core error with a stable type and diagnostic message.
#[derive(Debug)]
pub struct IdentityError {
    type_: IdentityErrorType,
    message: Box<str>,
    refusal: Option<IdentityRefusal>,
    source: Option<wamn_envelope::encryption::Error>,
}

impl IdentityError {
    pub fn new(type_: IdentityErrorType, message: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            message: message.into(),
            refusal: None,
            source: None,
        }
    }

    /// Create an error with the refusal reason that a route answers to its caller.
    pub fn refused(
        type_: IdentityErrorType,
        refusal: IdentityRefusal,
        message: impl Into<Box<str>>,
    ) -> Self {
        Self {
            refusal: Some(refusal),
            ..Self::new(type_, message)
        }
    }

    /// Return the stable failure class.
    pub const fn error_type(&self) -> IdentityErrorType {
        self.type_
    }

    /// Why the input was refused, when a write names it.
    pub const fn refusal(&self) -> Option<IdentityRefusal> {
        self.refusal
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &dyn std::error::Error)
    }
}

impl From<wamn_envelope::encryption::Error> for IdentityError {
    fn from(source: wamn_envelope::encryption::Error) -> Self {
        Self {
            source: Some(source),
            ..Self::new(
                IdentityErrorType::Encryption,
                "session signing key envelope operation failed",
            )
        }
    }
}

/// Translate protocol input errors at the identity boundary.
impl From<wamn_identity_protocol::ParseIdentityError> for IdentityError {
    fn from(source: wamn_identity_protocol::ParseIdentityError) -> Self {
        if source.field() == "principal_id" {
            Self::refused(
                IdentityErrorType::InvalidInput,
                IdentityRefusal::Invalid("principal_id"),
                source.to_string(),
            )
        } else {
            Self::new(IdentityErrorType::InvalidInput, source.to_string())
        }
    }
}

impl From<wamn_identity_protocol::secret::TokenError> for IdentityError {
    fn from(source: wamn_identity_protocol::secret::TokenError) -> Self {
        Self::new(IdentityErrorType::Entropy, source.to_string())
    }
}

/// Classify an identity database failure and retain its public diagnostic.
pub fn database_error(error: &sqlx::Error) -> IdentityError {
    let type_ = match error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .as_deref()
    {
        Some("23505") => IdentityErrorType::Conflict,
        Some("23503") => IdentityErrorType::NotFound,
        _ => IdentityErrorType::Database,
    };
    // `sqlx::Error` displays as the bare string "db error", so the
    // readable half is taken from its database error. That value's own Display
    // carries PostgreSQL's DETAIL, which repeats the offending key, so the
    // message and the constraint name are read directly instead. An operator
    // learns which rule refused the write, and the row's data stays out of the
    // log.
    let reason = error.as_database_error().map_or_else(
        || error.to_string(),
        |database| match database.constraint() {
            Some(constraint) => format!("{} ({constraint})", database.message()),
            None => database.message().to_owned(),
        },
    );
    IdentityError::new(
        type_,
        format!("platform identity database operation failed: {reason}"),
    )
}
