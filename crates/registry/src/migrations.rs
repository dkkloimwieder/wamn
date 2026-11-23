//! Registry migration history access.

/// Registry migration history.
pub trait MigrationHistory {
    type Error;

    /// Read whether the system migration history exists.
    ///
    /// # Errors
    /// Returns database errors.
    fn history_exists(
        &mut self,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send;
}
