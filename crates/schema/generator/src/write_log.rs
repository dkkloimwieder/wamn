//! The three statements of the write log, `app_system.write_log`
//! (`docs/plan/write-log.md` 4.2).
//!
//! Their text is fixed. The generator emits one copy of each per package and
//! lists all three in the contract of every claim operation, so every claim
//! operation carries the same three digests. The relation is schema-qualified
//! because `deploy/sql/app-schema.sql` installs it outside every package schema.

use std::collections::BTreeMap;

use crate::GenerateError;

/// The package directory that holds the three statement files.
pub const WRITE_LOG_DIRECTORY: &str = "generated/sql/write_log";
/// The schema of the write log.
pub const WRITE_LOG_SCHEMA: &str = "app_system";
/// The table of the write log.
pub const WRITE_LOG_TABLE: &str = "write_log";

/// The contract name of the claim statement.
pub const LOG_CLAIM: &str = "log_claim";
/// The contract name of the read statement.
pub const LOG_READ: &str = "log_read";
/// The contract name of the finish statement.
pub const LOG_FINISH: &str = "log_finish";

/// Insert the claim of one key, or nothing when a committed row holds the key.
///
/// An uncommitted row of another transaction makes this insert wait until that
/// transaction ends. A commit makes it return no row, and a rollback lets it
/// insert.
pub const LOG_CLAIM_SQL: &str = "INSERT INTO app_system.write_log (operation, idempotency_key, request)\nVALUES ($1::text, $2::text, $3::bytea)\nON CONFLICT (operation, idempotency_key) DO NOTHING\nRETURNING idempotency_key;\n";

/// Read the request and the result that a committed claim of one key holds.
pub const LOG_READ_SQL: &str = "SELECT\n    request,\n    result\nFROM app_system.write_log\nWHERE operation = $1::text\n  AND idempotency_key = $2::text;\n";

/// Store the result of the work in the claim of this transaction.
pub const LOG_FINISH_SQL: &str = "UPDATE app_system.write_log\nSET result = $3::text\nWHERE operation = $1::text\n  AND idempotency_key = $2::text\n  AND result IS NULL\nRETURNING idempotency_key;\n";

/// The three statements as `(name, file stem, text)`, in contract order.
pub const WRITE_LOG_STATEMENTS: [(&str, &str, &str); 3] = [
    (LOG_CLAIM, "claim", LOG_CLAIM_SQL),
    (LOG_READ, "read", LOG_READ_SQL),
    (LOG_FINISH, "finish", LOG_FINISH_SQL),
];

/// Whether `path` is one of the three statement files.
///
/// Their text is the platform's, and they name a relation outside the package
/// schemas, so the package statement check does not plan them. The live test
/// of the write log checks them as `wamn_app` instead.
pub(crate) fn is_write_log_path(path: &str) -> bool {
    path.strip_prefix(WRITE_LOG_DIRECTORY)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The write log operation of one contract operation: the operation without
/// its `@version`, so a retry across a release answers the stored result.
pub(crate) fn log_operation(operation_id: &str) -> &str {
    operation_id
        .rsplit_once('@')
        .map_or(operation_id, |(operation, _)| operation)
}

/// Emit the three statement files of one package, and the codec helper that
/// every claim codec of the package includes.
///
/// The files join the package SQL corpus, because the claim operations list
/// them: the corpus identity covers every statement a contract admits.
pub(crate) fn emit(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), GenerateError> {
    for (_, stem, sql) in WRITE_LOG_STATEMENTS {
        let path = format!("{WRITE_LOG_DIRECTORY}/{stem}.sql");
        crate::generate::insert_bytes(files, &path, sql.as_bytes().to_vec())?;
        if sql_corpus
            .insert(path.clone(), sql.as_bytes().to_vec())
            .is_some()
        {
            return Err(GenerateError::for_path(
                crate::GenerateErrorKind::DuplicatePath,
                "the write log SQL collides with the corpus",
                path,
            ));
        }
    }
    crate::generate::insert_bytes(
        files,
        "generated/wit/write_log_codec.rs",
        codec_source().into_bytes(),
    )
}

/// The write log calls of a claim codec, over the three statement digests.
fn codec_source() -> String {
    format!(
        r#"// @generated from the write log statements; do not edit.

const LOG_CLAIM_DIGEST: &str = {claim:?};
const LOG_READ_DIGEST: &str = {read:?};
const LOG_FINISH_DIGEST: &str = {finish:?};

/// What the write log holds for one key after this transaction's claim.
enum Logged {{
    /// This transaction holds the claim.
    Claimed,
    /// A committed claim holds the key, with its request and its result.
    Stored(Vec<u8>, Option<String>),
}}

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
) -> Result<Logged, wamn_postgres_statements::StatementError> {{
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
    let claimed = wamn_postgres_statements::decode_optional(LOG_CLAIM_DIGEST, rows, |row| {{
        row.decode::<String>("idempotency_key")
    }})?;
    if claimed.is_some() {{
        return Ok(Logged::Claimed);
    }}
    let rows = transaction
        .run(
            LOG_READ_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(operation.to_owned()),
                wamn_postgres_statements::into_sql_value(key.to_owned()),
            ],
        )
        .await?;
    let (request, result) = wamn_postgres_statements::decode_one(LOG_READ_DIGEST, rows, |row| {{
        Ok((row.decode::<Vec<u8>>("request")?, row.decode::<Option<String>>("result")?))
    }})?;
    Ok(Logged::Stored(request, result))
}}

/// Store the result of the work in this transaction's claim.
async fn log_finish(
    transaction: &mut wamn_postgres_statements::Transaction,
    operation: &str,
    key: &str,
    result: String,
) -> Result<(), wamn_postgres_statements::StatementError> {{
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
    wamn_postgres_statements::decode_one(LOG_FINISH_DIGEST, rows, |row| {{
        row.decode::<String>("idempotency_key")
    }})
    .map(|_| ())
}}

/// The error literal of a write log statement, its transaction or its commit.
fn log_error(error: &wamn_postgres_statements::StatementError) -> &'static str {{
    match error.kind() {{
        wamn_postgres_statements::StatementErrorKind::SerializationFailure
        | wamn_postgres_statements::StatementErrorKind::ConnectionUnavailable => "retry",
        wamn_postgres_statements::StatementErrorKind::StatementTimeout => "timeout",
        _ => "internal_error",
    }}
}}
"#,
        claim = crate::generate::sha256(LOG_CLAIM_SQL.as_bytes()),
        read = crate::generate::sha256(LOG_READ_SQL.as_bytes()),
        finish = crate::generate::sha256(LOG_FINISH_SQL.as_bytes()),
    )
}
