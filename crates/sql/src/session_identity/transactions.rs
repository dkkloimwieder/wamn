use sqlx::Statement as _;

use tracing::Instrument as _;

use wamn_event_wire::Causation;
use wamn_identity_protocol::AuthorityClass;

use super::super::PgError;
use super::super::types::map_pg_error;
use super::{WamnPostgres, validate_session_identity};

pub(in super::super) fn prepare_causation(run: &Causation) -> (String, String) {
    let statement = "SELECT pg_logical_emit_message(true, 'wamn.causation', $1::text)".to_owned();
    let json = serde_json::to_string(run).expect("Causation serializes to JSON");
    (statement, json)
}

pub(in super::super) async fn finish_transaction(
    conn: &mut sqlx::PgConnection,
    verb: &str,
) -> Result<(), sqlx::Error> {
    if verb == "COMMIT" {
        let statement = sqlx::Executor::prepare(
            &mut *conn,
            sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe((verb).to_string())),
        )
        .await?;
        sqlx::query(statement.sql().clone())
            .execute(&mut *conn)
            .await
            .map(|result| result.rows_affected())
            .map(drop)
    } else {
        sqlx::raw_sql(sqlx::AssertSqlSafe((verb).to_string()))
            .execute(&mut *conn)
            .await
            .map(|_| ())
    }
}

/// The first statement after BEGIN records the caller for this transaction.
/// Return the xid expression without reading a column: the login has INSERT only.
pub(super) const IDENTITY_SQL: &str = "INSERT INTO platform.tx_identity \
    (xid, principal_id, role, application, operation, runner) \
    VALUES (pg_current_xact_id(), $1, $2, $3, $4, $5) RETURNING pg_current_xact_id()::text";

/// Open a host write transaction before inserting its caller.
const BEGIN_READ_COMMITTED: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";

/// Open the snapshot before inserting its caller and switching to READ ONLY.
const BEGIN_READ_SNAPSHOT: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ";

/// Prepared caller identity inserted immediately after BEGIN.
#[derive(Debug)]
pub(in super::super) struct BoundIdentity {
    begin: &'static str,
    pub(in super::super) causation: Option<(String, String)>,
    principal: wamn_identity_protocol::PrincipalId,
    params: Vec<Option<String>>,
    schema: Option<String>,
    statement_timeout_ms: u32,
    read: bool,
    class: AuthorityClass,
}

impl BoundIdentity {
    pub(in super::super) async fn bind(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<String, PgError> {
        async {
            sqlx::query(sqlx::AssertSqlSafe((self.begin).to_string()))
                .execute(&mut *conn)
                .await
                .map_err(|e| map_pg_error(&e))?;
            let mut query = sqlx::query_scalar::<_, String>(IDENTITY_SQL).bind(self.principal);
            for value in &self.params {
                query = query.bind(value);
            }
            let xid = query.fetch_one(&mut *conn).await.map_err(|e| map_pg_error(&e))?;
            if self.read {
                sqlx::query("SET TRANSACTION READ ONLY")
                    .execute(&mut *conn).await.map_err(|e| map_pg_error(&e))?;
            }
            sqlx::query("SELECT set_config('statement_timeout', $1, true), set_config('search_path', COALESCE($2, current_setting('search_path')), true)")
                .bind(self.statement_timeout_ms.to_string())
                .bind(&self.schema)
                .execute(&mut *conn).await.map_err(|e| map_pg_error(&e))?;
            if let Some((statement, json)) = &self.causation {
                sqlx::query(sqlx::AssertSqlSafe(statement.clone()))
                    .bind(json).execute(&mut *conn).await.map_err(|e| map_pg_error(&e))?;
            }
            Ok(xid)
        }
        .instrument(tracing::info_span!(
            "wamn.postgres.bind_identity",
            wamn.authority_class = self.class.as_str()
        ))
        .await
    }
}

impl WamnPostgres {
    /// Begin a transaction and bind its caller before any application SQL.
    #[expect(
        clippy::too_many_arguments,
        reason = "every identity this transaction binds is an independently trusted input"
    )]
    pub(in super::super) async fn begin_with_identity(
        &self,
        conn: &mut sqlx::PgConnection,
        class: AuthorityClass,
        read: bool,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&wamn_identity_protocol::PrincipalId>,
        operation: Option<&str>,
        application: Option<&wamn_catalog::ApplicationName>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<String, PgError> {
        Self::prepare_identity(
            class,
            read,
            schema,
            runner,
            role,
            user_id,
            operation,
            application,
            run,
            statement_timeout_ms,
        )?
        .bind(conn)
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "every identity this transaction binds is an independently trusted input"
    )]
    pub(in super::super) fn prepare_identity(
        class: AuthorityClass,
        read: bool,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&wamn_identity_protocol::PrincipalId>,
        operation: Option<&str>,
        application: Option<&wamn_catalog::ApplicationName>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<BoundIdentity, PgError> {
        // The project selected this connection credential. Refuse malformed values
        // before opening its transaction.
        validate_session_identity(schema, runner, role, operation)?;
        let principal = user_id.ok_or_else(|| {
            PgError::QueryError(("55000".to_owned(), "actor-required".to_owned()))
        })?;
        let params = vec![
            Some(role.unwrap_or_default().to_owned()),
            Some(
                application
                    .map_or("", wamn_catalog::ApplicationName::as_str)
                    .to_owned(),
            ),
            operation.map(str::to_owned),
            runner.map(str::to_owned),
        ];
        // The isolation level is stated, not inherited from a session default:
        // the write log's retry reads the identity that its insert waited on, and
        // only a fresh READ COMMITTED snapshot sees it
        // (docs/architecture/execution.md#native-dispatch). A host read
        // transaction (`read`) reads one snapshot.
        let begin = if read {
            BEGIN_READ_SNAPSHOT
        } else {
            BEGIN_READ_COMMITTED
        };
        let causation = run.filter(|_| !read).map(prepare_causation);
        Ok(BoundIdentity {
            begin,
            causation,
            principal: *principal,
            params,
            schema: schema.map(str::to_owned),
            statement_timeout_ms,
            read,
            class,
        })
    }
}
