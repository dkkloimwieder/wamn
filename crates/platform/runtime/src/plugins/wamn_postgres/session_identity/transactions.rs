use sqlx::{Row as _, Statement as _};
use std::sync::Arc;

use tracing::Instrument as _;

use wamn_event_wire::Causation;
use wamn_run_state::AuthorityClass;

use super::super::pool::ProjectPool;
use super::super::resources::{
    StatementConnectionGuard, run_execute, run_query, run_verified_query,
};
use super::super::statements::VerifiedStatement;
use super::super::types::map_pg_error;
use super::super::{PgError, RowSet, SqlValue, StatementError};
use super::{
    OneShotResult, WamnPostgres, refuse_unattributed_statement, validate_session_identity,
};

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

/// The fully-bound identity statement run inside the plugin-managed transaction
/// (R2/R16). Every identity value travels as a bind parameter (`$1..$8`) — there is
/// NO string-interpolation path, so an injection-shaped project / schema / runner
/// / role / user id is *unrepresentable* as SQL, not merely rejected by
/// validation. `set_config` with `is_local => true` is the exact `SET LOCAL`
/// equivalent (transaction-local). Parameter order:
///
/// - `$1` `app.project_env_key` — the RLS identity (always present).
/// - `$2` `statement_timeout` — as TEXT (a bare-integer string = milliseconds).
/// - `$3` `search_path` — `COALESCE($3, current_setting('search_path'))`, so a
///   NULL bind (absent schema) preserves the server's default search_path; the
///   S2/pgbench path is byte-unchanged.
/// - `$4` `app.runner` — `COALESCE($4, current_setting('app.runner', true))`, so
///   a NULL bind (absent runner) re-asserts the current value (a no-op), exactly
///   like the pre-fqg.4 "no `app.runner` statement" path.
/// - `$5` `app.role` / `$6` `app.user_id` — the per-role RLS identity the
///   compiled policies key on (wamn-0h0g.23.1), and the executing principal
///   the record-history triggers stamp. Bound UNCONDITIONALLY, not
///   COALESCEd to the current value like `$3`/`$4`: an absent identity binds `''`,
///   which is exactly the base denial policy
///   `COALESCE(current_setting('app.role', true), '')` and
///   `NULLIF(current_setting('app.user_id', true), '')::uuid` use in the static
///   application-schema RLS contract. Re-asserting whatever the
///   pooled connection currently carries would let a session-level value survive
///   into the next component's transaction, turning a shared connection into a
///   role escalation; binding the minimum cannot.
/// - `$7` `app.operation` is the executing operation token, or
///   `wamn:<component>` for a platform component outside a registered
///   operation. The record-history log records it as the source of a write.
///   It binds unconditionally like `$6`. An absent operation binds `''`, so a
///   pooled connection never carries the operation of an earlier request.
/// - `$8` `app.application` is the declared application that runs the
///   operation, named by the environment map entry that holds its package.
///   Base operations use that map too. It is for logging only:
///   guest SQL can rewrite it, so numbering reads the application from host
///   state instead (docs/architecture/data-access.md#rendering). It binds
///   unconditionally like `$7`, and an absent application binds `''`.
///
/// The `wamn.causation` emit (l5i9.12.2) is NOT part of this statement — it is a
/// separate prepared emit pipelined by [`begin_with_identity`]
/// only for a run-owned transaction.
pub(super) const IDENTITY_SQL: &str = "SELECT \
     set_config('app.project_env_key', $1, true), \
     set_config('statement_timeout', $2, true), \
     set_config('search_path', COALESCE($3, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($4, current_setting('app.runner', true)), true), \
     set_config('app.role', $5, true), \
     set_config('app.user_id', $6, true), \
     set_config('app.operation', $7, true), \
     set_config('app.application', $8, true)";

/// The GUEST identity statement: [`IDENTITY_SQL`] WITHOUT `app.project_env_key`
/// (`wamn-0h0g.22.6.7`).
///
/// *** THE GUEST'S PROJECT IS ITS LOGIN, NOT A IDENTITY. *** Every relation the
/// guest can reach now keys on
/// `wamn_authority.project_key(project_env_key) = wamn_authority.current_project_key()`,
/// which reads `current_user` — so injecting `app.project_env_key` here would set a GUC
/// that nothing the guest can read consults. Leaving it would not be belt and
/// braces; it would be a second, SETTABLE statement about an authority the
/// session no longer derives that way, and the next person to add a policy
/// would have two boundaries to choose between.
///
/// `app.role` and `app.user_id` STAY. They key the RESTRICTIVE per-role and
/// per-user policies, a different identity class layered INSIDE the project base grants
/// and explicitly outside the work of `wamn-0h0g.22.6`. `app.operation` stays with
/// them: the record-history log records it. `app.application` stays too,
/// for logging only.
/// Apply connection configuration before an autocommit read.
pub(in super::super) async fn prepare_session_settings(
    connection: &mut sqlx::PgConnection,
    _policy: &ProjectPool,
    _schema: Option<&str>,
    _runner: Option<&str>,
) -> Result<Option<sqlx::postgres::PgStatement>, PgError> {
    sqlx::Executor::prepare(&mut *connection, sqlx::SqlSafeStr::into_sql_str("SELECT set_config('search_path', COALESCE($1, (SELECT reset_val FROM pg_settings WHERE name='search_path')), false), set_config('app.runner', COALESCE($2, ''), false)"))
        .await.map(Some).map_err(|error| map_pg_error(&error))
}

pub(in super::super) async fn bind_session_settings(
    connection: &mut sqlx::PgConnection,
    _policy: &ProjectPool,
    prepared: Option<&sqlx::postgres::PgStatement>,
    schema: Option<String>,
    runner: Option<String>,
) -> Result<(), PgError> {
    if let Some(statement) = prepared {
        sqlx::query(statement.sql().clone())
            .bind(schema)
            .bind(runner)
            .execute(&mut *connection)
            .await
            .map_err(|error| map_pg_error(&error))?;
    }
    Ok(())
}

const GUEST_IDENTITY_SQL: &str = "SELECT \
     set_config('statement_timeout', $1, true), \
     set_config('search_path', COALESCE($2, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($3, current_setting('app.runner', true)), true), \
     set_config('app.role', $4, true), \
     set_config('app.user_id', $5, true), \
     set_config('app.operation', $6, true), \
     set_config('app.application', $7, true)";

/// The bound identity statement one authority class binds. ONE function, so the
/// pipelined path's warm-up and the transaction that follows it cannot disagree
/// about which cache entry they mean (wamn-0h0g.17.33).
const fn identity_sql(class: AuthorityClass) -> &'static str {
    match class {
        AuthorityClass::GuestSql | AuthorityClass::DynamicSql => GUEST_IDENTITY_SQL,
        _ => IDENTITY_SQL,
    }
}

/// The statement that opens every bound transaction of this plugin, except
/// a guest transaction of a read invocation.
const BEGIN_READ_COMMITTED: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";

/// The statement that opens a guest transaction of a read invocation, so every
/// statement in it reads one snapshot and none writes.
const BEGIN_READ_SNAPSHOT: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY";

/// Prepared identity whose first poll sends BEGIN before the bound settings.
#[derive(Debug)]
pub(in super::super) struct BoundIdentity {
    begin: &'static str,
    pub(in super::super) causation: Option<(String, String)>,
    statement: &'static str,
    params: Vec<Option<String>>,
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
            if let Some((statement, json)) = &self.causation {
                sqlx::query(sqlx::AssertSqlSafe((statement).clone()))
                    .bind(json)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| map_pg_error(&e))?;
            }
            let mut query = sqlx::query(sqlx::AssertSqlSafe((self.statement).to_string()));
            for value in &self.params {
                query = query.bind(value);
            }
            query
                .execute(&mut *conn)
                .await
                .map_err(|e| map_pg_error(&e))?;
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
    /// `BEGIN` + identity/limit injection. The identity are injected by ONE fully
    /// bound statement ([`IDENTITY_SQL`]) whose every value travels as a bind
    /// parameter — there is no interpolation path (R2/R16). `project_env_key` is always
    /// present; `schema`/`runner` bind NULL when absent (COALESCE-to-current
    /// preserves the server default / prior value — the S2/pgbench path is
    /// byte-unchanged), and `role`/`user_id` bind `''` when absent, the deny
    /// minimum the compiled RLS predicates read. An absent `operation` or
    /// `application` also binds `''`. A run-owned transaction also
    /// appends the transactional `wamn.causation` emit (l5i9.12.2).
    ///
    /// The SQLx session begins the transaction before binding its local identity.
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
        // The project is still VALIDATED on the guest path even though it is no
        // longer bound: it selected the credential this connection was validated
        // out with, so a malformed one is a bug worth failing on.
        validate_session_identity(
            project_env_key,
            schema,
            runner,
            role,
            user_id,
            operation,
            application,
        )?;
        let guest = matches!(class, AuthorityClass::GuestSql | AuthorityClass::DynamicSql);
        // Prepare every statement before a caller polls the pipeline. A cache
        // miss must not let the application statement run ahead of BEGIN.
        let stmt = identity_sql(class);
        let mut params = vec![
            Some(statement_timeout_ms.to_string()),
            schema.map(str::to_owned),
            runner.map(str::to_owned),
            Some(role.unwrap_or_default().to_owned()),
            Some(user_id.unwrap_or_default().to_owned()),
            Some(operation.unwrap_or_default().to_owned()),
            Some(application.unwrap_or_default().to_owned()),
        ];
        if !guest {
            params.insert(0, Some(project_env_key.to_owned()));
        }
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
        let causation = run.map(prepare_causation);
        Ok(BoundIdentity {
            begin,
            causation,
            statement: stmt,
            params,
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
        want_rows: bool,
    ) -> Result<OneShotResult, PgError> {
        let project = self.project_for(component_id);
        self.one_shot_for_project(component_id, &project, sql, params, want_rows)
            .await
    }

    /// Execute one statement using an explicitly selected named-import
    /// project. Named `wamn:postgres` interfaces must not fall back to the
    /// component's single `wamn.project` identity.
    pub(in super::super) async fn one_shot_for_project(
        &self,
        component_id: &str,
        project: &str,
        sql: &str,
        params: &[SqlValue],
        want_rows: bool,
    ) -> Result<OneShotResult, PgError> {
        let project_env_key = self.require_project_env_key(component_id)?;
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let role = self.role_for(component_id);
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let application = self.application_for(component_id);
        let run = self.current_run_for(component_id);
        let (mut conn, pp) = self.checkout_dynamic(project, &project_env_key).await?;
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
        let result = if want_rows {
            run_query(&mut conn, sql, params, pp.row_limit)
                .await
                .map(OneShotResult::Rows)
        } else {
            run_execute(&mut conn, sql, params)
                .await
                .map(OneShotResult::Count)
        };
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
        // AUTOCOMMIT WHEN THE SERVER SAYS NO TRANSACTION IS NEEDED. PostgreSQL
        // classified this statement at generation time: it neither writes nor
        // takes a row lock, so BEGIN and COMMIT are ceremony around a read.
        // Measured cost of that ceremony: bind_identity 0.45-0.89 ms plus a
        // COMMIT of 2.1-3.8 ms, around a 0.6 ms statement
        // (measurement records in commits 494b5b8d5595 and 3659506bdeef).
        //
        // A read carrying a role identity keeps the transaction: a session-wide
        // app.role would outlive the request and reach the next borrower of
        // this pooled connection. A bound executing principal or operation does
        // not: the read skips app.user_id and app.operation, which only the
        // record-history triggers read.
        if !statement.transactional && role.is_none() {
            let prepared = prepare_session_settings(
                connection.connection(),
                &policy,
                schema.as_deref(),
                runner.as_deref(),
            )
            .await
            .map_err(StatementError::Postgres)?;
            bind_session_settings(
                connection.connection(),
                &policy,
                prepared.as_ref(),
                schema,
                runner,
            )
            .await
            .map_err(StatementError::Postgres)?;
            let wal = read_wal_position(connection.connection(), wal_statement.as_ref()).await;
            let rows = run_verified_query(
                connection.connection(),
                digest,
                statement,
                binds,
                policy.row_limit,
            )
            .await;
            let mut rows = rows?;
            rows.wal_position = wal?;
            connection.repool();
            return Ok(rows);
        }

        // Bind the transaction identity before running the application statement.
        let identity = Self::prepare_identity(
            authority,
            false,
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
        let result = result.and_then(|mut rows| {
            rows.wal_position = wal?;
            Ok(rows)
        });
        match result {
            // COMMIT is a SECOND full server round trip on every request, and it
            // sat inside wamn.postgres with no span: statement ended at 13.7 ms
            // and the postgres span at 15.9 ms, so 2.1 ms was invisible here.
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
