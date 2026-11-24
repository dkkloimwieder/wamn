//! Convert PostgreSQL errors to the application SQL contract.

use wamn_application::database::SqlError;

/// Keep the database message and constraint and omit row-bearing DETAIL and HINT.
pub fn postgres_error_detail(error: &sqlx::Error) -> String {
    match error.as_database_error() {
        Some(database) => match database.constraint() {
            Some(constraint) => format!("{} ({constraint})", database.message()),
            None => database.message().to_owned(),
        },
        None => error.to_string(),
    }
}

/// Map a PostgreSQL failure to the SQL contract.
pub fn map_pg_error(error: &sqlx::Error) -> SqlError {
    if let Some(database) = error.as_database_error() {
        let constraint = || database.constraint().unwrap_or_default().to_owned();
        return match database.code().as_deref().unwrap_or("XX000") {
            "40001" | "40P01" => SqlError::SerializationFailure,
            "57014" => SqlError::StatementTimeout,
            "23505" => SqlError::UniqueViolation(constraint()),
            "23503" => SqlError::ForeignKeyViolation(constraint()),
            "23514" => SqlError::CheckViolation(constraint()),
            "23P01" => SqlError::ExclusionViolation(constraint()),
            "42501" => SqlError::PermissionDenied,
            code => SqlError::QueryError((code.to_owned(), database.message().to_owned())),
        };
    }
    if connection_lost(error) {
        return SqlError::ConnectionUnavailable;
    }
    SqlError::QueryError(("XX000".into(), error.to_string()))
}

/// Check if the connection can no longer run SQL.
pub fn connection_lost(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_)
            | sqlx::Error::PoolClosed
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::WorkerCrashed
    )
}
