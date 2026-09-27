// @generated from the write log statements; do not edit.

const LOG_CLAIM_DIGEST: &str =
    "sha256:8b64e31699dc257736340dc6136a05c13888290e76d55df4b1b6726d7ac1e6be";
const LOG_READ_DIGEST: &str =
    "sha256:fb83c78819d4b5e8d6da72e63c6cc778172314775bd0fa378e8e26a9fa4647cd";
const LOG_FINISH_DIGEST: &str =
    "sha256:8b9c81bb1c9c381c49b4aba44a9cfc4e2402e7146c925515c1b2ee3bf62b971b";

/// What the write log holds for one key after this transaction's claim.
enum Logged {
    /// This transaction holds the claim.
    Claimed,
    /// A committed claim holds the key, with its request and its result.
    Stored(Vec<u8>, Option<String>),
}

/// Claim one key, or read what the committed claim of the key holds.
///
/// A claim that another open transaction holds makes the insert wait for it.
/// The read that follows a lost claim takes a fresh snapshot under
/// `READ COMMITTED`, so it sees the row the insert waited on.
async fn log_claim(
    transaction: &mut wamn_postgres_statements::Transaction,
    operation: &str,
    key: &str,
    request: &[u8],
) -> Result<Logged, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOG_CLAIM_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(operation.to_owned()),
                wamn_postgres_statements::into_sql_value(key.to_owned()),
                wamn_postgres_statements::into_sql_value(request.to_vec()),
            ],
        )
        .await?;
    let claimed = wamn_postgres_statements::decode_optional(LOG_CLAIM_DIGEST, rows, |row| {
        row.decode::<String>("idempotency_key")
    })?;
    if claimed.is_some() {
        return Ok(Logged::Claimed);
    }
    let rows = transaction
        .run(
            LOG_READ_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(operation.to_owned()),
                wamn_postgres_statements::into_sql_value(key.to_owned()),
            ],
        )
        .await?;
    let (request, result) = wamn_postgres_statements::decode_one(LOG_READ_DIGEST, rows, |row| {
        Ok((
            row.decode::<Vec<u8>>("request")?,
            row.decode::<Option<String>>("result")?,
        ))
    })?;
    Ok(Logged::Stored(request, result))
}

/// Store the result of the work in this transaction's claim.
async fn log_finish(
    transaction: &mut wamn_postgres_statements::Transaction,
    operation: &str,
    key: &str,
    result: String,
) -> Result<(), wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOG_FINISH_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(operation.to_owned()),
                wamn_postgres_statements::into_sql_value(key.to_owned()),
                wamn_postgres_statements::into_sql_value(result),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(LOG_FINISH_DIGEST, rows, |row| {
        row.decode::<String>("idempotency_key")
    })
    .map(|_| ())
}

/// The error literal of a write log statement, its transaction or its commit.
fn log_error(error: &wamn_postgres_statements::StatementError) -> &'static str {
    match error.kind() {
        wamn_postgres_statements::StatementErrorKind::SerializationFailure
        | wamn_postgres_statements::StatementErrorKind::ConnectionUnavailable => "retry",
        wamn_postgres_statements::StatementErrorKind::StatementTimeout => "timeout",
        _ => "internal_error",
    }
}
