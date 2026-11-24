//! Statements shared by platform installation and project reservations.

/// Serialize writes to the project stream limit and reservations against it.
pub const PROJECT_STREAM_CAPACITY_LOCK_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtextextended('wamn.project-stream-capacity', 0))";
