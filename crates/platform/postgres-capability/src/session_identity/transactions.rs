use sqlx::{Row as _, Statement as _};
use std::sync::Arc;

use tracing::Instrument as _;

use wamn_event_wire::Causation;
use wamn_identity_protocol::AuthorityClass;

use super::super::resources::{StatementConnectionGuard, run_query, run_verified_query};
use super::super::statements::VerifiedStatement;
use super::super::types::map_pg_error;
use super::super::{PgError, RowSet, SqlValue, StatementError};
use super::{WamnPostgres, refuse_unattributed_statement, validate_session_identity};

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
pub(super) const IDENTITY_SQL: &str = "INSERT INTO platform.tx_identity \
    (xid, principal_id, role, application, operation, runner) \
    VALUES (pg_current_xact_id(), $1::text::uuid, $2, $3, $4, $5)";

/// The statement that opens every bound transaction of this plugin, except
/// a guest transaction of a read invocation.
const BEGIN_READ_COMMITTED: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";

/// Open the snapshot before inserting its caller and switching to READ ONLY.
const BEGIN_READ_SNAPSHOT: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ";

/// Prepared caller identity inserted immediately after BEGIN.
#[derive(Debug)]
pub(in super::super) struct BoundIdentity {
    begin: &'static str,
    pub(in super::super) causation: Option<(String, String)>,
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
    ) -> Result<(), PgError> {
        async {
            sqlx::query(sqlx::AssertSqlSafe((self.begin).to_string()))
                .execute(&mut *conn)
                .await
                .map_err(|e| map_pg_error(&e))?;
            let mut query = sqlx::query(IDENTITY_SQL);
            for value in &self.params {
                query = query.bind(value);
            }
            query.execute(&mut *conn).await.map_err(|e| map_pg_error(&e))?;
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
            Ok(())
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
        project_env_key: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&str>,
        operation: Option<&str>,
        application: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<(), PgError> {
        Self::prepare_identity(
            class,
            read,
            project_env_key,
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
        project_env_key: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&str>,
        operation: Option<&str>,
        application: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<BoundIdentity, PgError> {
        // The project selected this connection credential. Refuse malformed values
        // before opening its transaction.
        validate_session_identity(
            project_env_key,
            schema,
            runner,
            role,
            user_id,
            operation,
            application,
        )?;
        let principal = user_id.ok_or_else(|| {
            PgError::QueryError(("55000".to_owned(), "actor-required".to_owned()))
        })?;
        let params = vec![
            Some(principal.to_owned()),
            Some(role.unwrap_or_default().to_owned()),
            Some(application.unwrap_or_default().to_owned()),
            operation.map(str::to_owned),
            runner.map(str::to_owned),
        ];
        // The isolation level is stated, not inherited from a session default:
        // the write log's retry reads the identity that its insert waited on, and
        // only a fresh READ COMMITTED snapshot sees it
        // (docs/architecture/execution.md#native-dispatch). A guest has no way
        // to name the level through `begin`; the guest transaction of a read
        // invocation (`read`) reads one snapshot.
        let begin = if read {
            BEGIN_READ_SNAPSHOT
        } else {
            BEGIN_READ_COMMITTED
        };
        let causation = run.filter(|_| !read).map(prepare_causation);
        Ok(BoundIdentity {
            begin,
            causation,
            params,
            schema: schema.map(str::to_owned),
            statement_timeout_ms,
            read,
            class,
        })
    }

    /// Single statement in an implicit transaction: identity injected,
    /// committed on success, rolled back on statement failure.
    pub(in super::super) async fn one_shot(
        &self,
        component_id: &str,
        sql: &str,
        params: &[SqlValue],
    ) -> Result<RowSet, PgError> {
        let project = self.project_for(component_id);
        let project_env_key = self.require_project_env_key(component_id)?;
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let role = self.role_for(component_id);
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let application = self.application_for(component_id);
        let run = self.current_run_for(component_id);
        let (mut conn, pp) = self.checkout_dynamic(&project, &project_env_key).await?;
        if let Err(e) = self
            .begin_with_identity(
                &mut conn,
                AuthorityClass::DynamicSql,
                true,
                &project_env_key,
                schema.as_deref(),
                runner.as_deref(),
                role.as_deref(),
                user_id.as_deref(),
                operation.as_deref(),
                application.as_deref(),
                run.as_ref(),
                pp.statement_timeout_ms,
            )
            .await
        {
            // Identity injection failed: connection state is unknown — destroy.
            self.destroy(conn);
            return Err(e);
        }
        let result = run_query(&mut conn, sql, params, pp.row_limit).await;
        match result {
            Ok(v) => match finish_transaction(&mut conn, "COMMIT").await {
                Ok(()) => Ok(v),
                Err(e) => {
                    self.destroy(conn);
                    Err(map_pg_error(&e))
                }
            },
            Err(pg_err) => {
                // Statement failed; roll the implicit transaction back and
                // repool. If even ROLLBACK fails the connection is toast.
                if let Err(e) = sqlx::raw_sql("ROLLBACK")
                    .execute(&mut *conn)
                    .await
                    .map(|_| ())
                {
                    tracing::warn!(error = %e, "rollback after failed statement also failed; destroying connection");
                    self.destroy(conn);
                }
                Err(pg_err)
            }
        }
    }

    /// Execute one admitted statement in an implicit identity-aware transaction.
    /// Contract drift rolls the transaction back before any mutation commits.
    pub(in super::super) async fn one_shot_statement(
        &self,
        component_id: &str,
        digest: &str,
        statement: &VerifiedStatement,
        binds: &[SqlValue],
        want_wal: bool,
    ) -> Result<RowSet, StatementError> {
        let project = self.project_for(component_id);
        let project_env_key = self
            .require_project_env_key(component_id)
            .map_err(StatementError::Postgres)?;
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let role = self.role_for(component_id);
        let user_id = self.user_id_for(component_id);
        refuse_unattributed_statement(statement, user_id.as_deref())
            .map_err(StatementError::Postgres)?;
        let operation = self.operation_for(component_id);
        let application = self.application_for(component_id);
        let run = self.current_run_for(component_id);
        let (connection, policy, authority) = self
            .checkout_workload(component_id, &project, &project_env_key)
            .await
            .map_err(StatementError::Postgres)?;
        let mut connection = StatementConnectionGuard::new(connection, Arc::clone(&self.destroyed));
        let wal_statement = if want_wal {
            Some(
                sqlx::Executor::prepare(
                    connection.connection(),
                    sqlx::SqlSafeStr::into_sql_str("SELECT pg_current_wal_lsn()::text"),
                )
                .await
                .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?,
            )
        } else {
            None
        };
        // Bind the transaction identity before running the application statement.
        let identity = Self::prepare_identity(
            authority,
            self.active_read(component_id),
            &project_env_key,
            schema.as_deref(),
            runner.as_deref(),
            role.as_deref(),
            user_id.as_deref(),
            operation.as_deref(),
            application.as_deref(),
            run.as_ref(),
            policy.statement_timeout_ms,
        )
        .map_err(StatementError::Postgres)?;
        identity
            .bind(connection.connection())
            .await
            .map_err(StatementError::Postgres)?;
        let wal = read_wal_position(connection.connection(), wal_statement.as_ref()).await;
        let result = run_verified_query(
            connection.connection(),
            digest,
            statement,
            binds,
            policy.row_limit,
        )
        .await;
        let result = result.and_then(|rows| {
            let wal = wal?;
            self.scopes
                .update_existing::<crate::invocation::Authority, _>(component_id, |record| {
                    record.wal_position = wal;
                });
            Ok(rows)
        });
        match result {
            Ok(rows) => match async { finish_transaction(connection.connection(), "COMMIT").await }
                .instrument(tracing::info_span!("wamn.postgres.commit"))
                .await
            {
                Ok(()) => {
                    connection.repool();
                    Ok(rows)
                }
                Err(error) => Err(StatementError::Postgres(map_pg_error(&error))),
            },
            Err(error) => {
                if let Err(rollback_error) = sqlx::raw_sql("ROLLBACK")
                    .execute(connection.connection())
                    .await
                    .map(|_| ())
                {
                    tracing::warn!(
                        error = %rollback_error,
                        "rollback after failed verified statement also failed; destroying connection"
                    );
                } else {
                    connection.repool();
                }
                Err(error)
            }
        }
    }
}

/// Read WAL metadata before the application statement in its transaction.
pub(in super::super) async fn read_wal_position(
    conn: &mut sqlx::PgConnection,
    statement: Option<&sqlx::postgres::PgStatement>,
) -> Result<Option<String>, StatementError> {
    let Some(statement) = statement else {
        return Ok(None);
    };
    sqlx::query(statement.sql().clone())
        .fetch_one(&mut *conn)
        .await
        .and_then(|row| row.try_get(0))
        .map(Some)
        .map_err(|error| StatementError::Postgres(map_pg_error(&error)))
}
