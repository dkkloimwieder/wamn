/// Convert plain PostgreSQL text to a domain value, preserving the decode error.
///
/// # Errors
/// Returns a column decode error when the domain constructor refuses the stored value.
pub fn decode_text<T, E>(value: String, index: impl std::fmt::Debug) -> Result<T, sqlx::Error>
where
    T: TryFrom<String, Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    value
        .try_into()
        .map_err(|source| sqlx::Error::ColumnDecode {
            index: format!("{index:?}"),
            source: Box::new(source),
        })
}
