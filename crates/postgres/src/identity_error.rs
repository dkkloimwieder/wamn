//! Translate Postgres failures at the identity boundary.

use wamn_authentication::identity_error::{IdentityError, IdentityErrorType};

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
