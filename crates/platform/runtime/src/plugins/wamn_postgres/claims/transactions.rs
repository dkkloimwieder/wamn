use std::sync::Arc;

use deadpool_postgres::Object;
use tokio_postgres::types::ToSql;
use tracing::Instrument as _;

use wamn_event_wire::Causation;
use wamn_run_state::AuthorityClass;

use super::super::pool::{ProjectPool, SessionSettings};
use super::super::resources::{
    StatementConnectionGuard, run_execute, run_query, run_verified_query,
};
use super::super::statements::VerifiedStatement;
use super::super::types::map_pg_error;
use super::super::{PgError, RowSet, SqlValue, StatementError};
use super::{OneShotResult, WamnPostgres, refuse_unattributed_statement, validate_claims};

/// The transactional `wamn.causation` logical-message emit appended to a
/// run-owned transaction's BEGIN batch (l5i9.12.2). The [`Causation`] is
/// serialized canonically (`{"run":..,"root":..,"depth":..}` — the reader
/// deserializes with `deny_unknown_fields`) and SQL-escaped (single quotes
/// doubled) for safe literal embedding in the simple-query batch, which takes
/// no bind params. `transactional = true` so the message rides the txn's commit
/// at its own LSN; the reader (l5i9.12.1) buffers the whole txn and stamps this
/// onto every row event regardless of frame order.
#[cfg(test)]
pub(in super::super) fn causation_emit_sql(c: &Causation) -> String {
    let json = serde_json::to_string(c).expect("Causation serializes to JSON");
    let literal = wamn_pg_core::quote_literal(&json);
    format!(" SELECT pg_logical_emit_message(true, 'wamn.causation', {literal});")
}

pub(in super::super) async fn prepare_causation(
    conn: &Object,
    run: &Causation,
) -> Result<(tokio_postgres::Statement, String), tokio_postgres::Error> {
    let statement = conn
        .prepare_cached("SELECT pg_logical_emit_message(true, 'wamn.causation', $1::text)")
        .await?;
    let json = serde_json::to_string(run).expect("Causation serializes to JSON");
    Ok((statement, json))
}

pub(in super::super) async fn finish_transaction(
    conn: &Object,
    verb: &str,
) -> Result<(), tokio_postgres::Error> {
    if verb == "COMMIT" {
        let statement = conn.prepare_cached(verb).await?;
        conn.execute(&statement, &[]).await.map(drop)
    } else {
        conn.batch_execute(verb).await
    }
}

/// The fully-bound claim statement run inside the plugin-managed transaction
/// (R2/R16). Every claim value travels as a bind parameter (`$1..$7`) — there is
/// NO string-interpolation path, so an injection-shaped tenant / schema / runner
/// / role / user id is *unrepresentable* as SQL, not merely rejected by
/// validation. `set_config` with `is_local => true` is the exact `SET LOCAL`
/// equivalent (scoped to the current transaction). Parameter order:
///
/// - `$1` `app.tenant` — the RLS claim (always present).
/// - `$2` `statement_timeout` — as TEXT (a bare-integer string = milliseconds).
/// - `$3` `search_path` — `COALESCE($3, current_setting('search_path'))`, so a
///   NULL bind (absent schema) preserves the server's default search_path; the
///   S2/pgbench path is byte-unchanged.
/// - `$4` `app.runner` — `COALESCE($4, current_setting('app.runner', true))`, so
///   a NULL bind (absent runner) re-asserts the current value (a no-op), exactly
///   like the pre-fqg.4 "no `app.runner` statement" path.
/// - `$5` `app.role` / `$6` `app.user_id` — the per-role RLS claim the
///   compiled policies key on (wamn-0h0g.23.1), and the executing principal
///   the record-history triggers stamp. Bound UNCONDITIONALLY, not
///   COALESCEd to the current value like `$3`/`$4`: an absent claim binds `''`,
///   which is exactly the deny floor
///   `COALESCE(current_setting('app.role', true), '')` and
///   `NULLIF(current_setting('app.user_id', true), '')::uuid` use in the static
///   application-schema RLS contract. Re-asserting whatever the
///   pooled connection currently carries would let a session-level value survive
///   into the next component's transaction, turning a shared connection into a
///   role escalation; binding the floor cannot.
/// - `$7` `app.operation` is the executing operation token, or
///   `wamn:<component>` for a platform component outside a registered
///   operation. The record-history log records it as the source of a write.
///   It binds unconditionally like `$6`. An absent operation binds `''`, so a
///   pooled connection never carries the operation of an earlier request.
///
/// The `wamn.causation` emit (l5i9.12.2) is NOT part of this statement — it is a
/// separate prepared emit pipelined by [`begin_with_claims`]
/// only for a run-owned transaction.
pub(super) const CLAIM_SQL: &str = "SELECT \
     set_config('app.tenant', $1, true), \
     set_config('statement_timeout', $2, true), \
     set_config('search_path', COALESCE($3, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($4, current_setting('app.runner', true)), true), \
     set_config('app.role', $5, true), \
     set_config('app.user_id', $6, true), \
     set_config('app.operation', $7, true)";

/// The GUEST claim statement: [`CLAIM_SQL`] WITHOUT `app.tenant`
/// (`wamn-0h0g.22.6.7`).
///
/// *** THE GUEST'S TENANT IS ITS LOGIN, NOT A CLAIM. *** Every relation the
/// guest can reach now keys on
/// `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`,
/// which reads `current_user` — so injecting `app.tenant` here would set a GUC
/// that nothing the guest can read consults. Leaving it would not be belt and
/// braces; it would be a second, SETTABLE statement about an authority the
/// session no longer derives that way, and the next person to add a policy
/// would have two boundaries to choose between.
///
/// `app.role` and `app.user_id` STAY. They key the RESTRICTIVE per-role and
/// per-user policies, a different claim class layered INSIDE the tenant floor
/// and explicitly outside `wamn-0h0g.22.6`'s scope. `app.operation` stays with
/// them: the record-history log records it.
/// The SESSION-scoped settings an autocommit read still needs.
///
/// `search_path` and `statement_timeout` are not claims -- they are how the
/// connection resolves this package's unqualified relations and how long it may
/// run. A read outside a transaction cannot take them transaction-locally, and
/// without `search_path` every generated statement fails to resolve its own
/// tables. The timeout is uniform within a pool. Each connection remembers
/// its last schema and runner, and a change applies before the next read.
/// An absent schema restores the server default; an absent runner clears it.
///
/// `app.role`, `app.user_id`, and `app.operation` are deliberately ABSENT.
/// Those are per-caller claims, and a session-scoped claim would outlive the
/// request and reach the next borrower of the pooled connection -- the exact
/// leak the claim model exists to prevent. A request carrying `app.role` takes
/// the transactional path instead. A read needs no `app.user_id` and no
/// `app.operation`: only the record-history triggers read them, and a read
/// that writes nothing fires no trigger.
const GUEST_AUTOCOMMIT_SETTINGS_SQL: &str = "SELECT \
     set_config('statement_timeout', $1, false), \
     set_config('search_path', COALESCE($2, (SELECT reset_val FROM pg_settings WHERE name = 'search_path')), false), \
     set_config('app.runner', COALESCE($3, ''), false)";

/// Prepare changed connection configuration before the application Parse is polled.
pub(in super::super) async fn prepare_session_settings(
    connection: &Object,
    policy: &ProjectPool,
    schema: Option<&str>,
    runner: Option<&str>,
) -> Result<Option<tokio_postgres::Statement>, PgError> {
    let identity = Arc::downgrade(&connection.statement_cache);
    let unchanged = policy
        .session_settings
        .lock()
        .expect("session settings lock poisoned")
        .iter()
        .any(|settings| {
            settings.connection.ptr_eq(&identity)
                && settings.schema.as_deref() == schema
                && settings.runner.as_deref() == runner
        });
    if unchanged {
        return Ok(None);
    }
    connection
        .prepare_cached(GUEST_AUTOCOMMIT_SETTINGS_SQL)
        .await
        .map(Some)
        .map_err(|error| map_pg_error(&error))
}

/// Install only connection configuration. Caller claims stay transaction-local.
pub(in super::super) async fn bind_session_settings(
    connection: &Object,
    policy: &ProjectPool,
    prepared: Option<&tokio_postgres::Statement>,
    schema: Option<String>,
    runner: Option<String>,
) -> Result<(), PgError> {
    let Some(prepared) = prepared else {
        return Ok(());
    };
    let timeout = policy.statement_timeout_ms.to_string();
    let params: &[&(dyn ToSql + Sync)] = &[&timeout, &schema.as_deref(), &runner.as_deref()];
    connection
        .execute(prepared, params)
        .await
        .map_err(|error| map_pg_error(&error))?;
    let identity = Arc::downgrade(&connection.statement_cache);
    let mut settings = policy
        .session_settings
        .lock()
        .expect("session settings lock poisoned");
    settings
        .retain(|entry| entry.connection.strong_count() > 0 && !entry.connection.ptr_eq(&identity));
    settings.push(SessionSettings {
        connection: identity,
        schema,
        runner,
    });
    Ok(())
}

const GUEST_CLAIM_SQL: &str = "SELECT \
     set_config('statement_timeout', $1, true), \
     set_config('search_path', COALESCE($2, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($3, current_setting('app.runner', true)), true), \
     set_config('app.role', $4, true), \
     set_config('app.user_id', $5, true), \
     set_config('app.operation', $6, true)";

/// The bound claim statement one authority class binds. ONE function, so the
/// pipelined path's warm-up and the transaction that follows it cannot disagree
/// about which cache entry they mean (wamn-0h0g.17.33).
const fn claim_sql(class: AuthorityClass) -> &'static str {
    match class {
        AuthorityClass::GuestSql => GUEST_CLAIM_SQL,
        _ => CLAIM_SQL,
    }
}

/// The statement that opens every claimed transaction of this plugin, except
/// a guest transaction of a read invocation.
const BEGIN_READ_COMMITTED: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";

/// The statement that opens a guest transaction of a read invocation, so every
/// statement in it reads one snapshot and none writes.
const BEGIN_READ_SNAPSHOT: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY";

/// Prepared claims whose first poll sends BEGIN before the bound settings.
#[derive(Debug)]
pub(in super::super) struct BoundClaims {
    begin: tokio_postgres::Statement,
    pub(in super::super) causation: Option<(tokio_postgres::Statement, String)>,
    statement: tokio_postgres::Statement,
    params: Vec<Option<String>>,
    class: AuthorityClass,
}

impl BoundClaims {
    pub(in super::super) async fn bind(&self, conn: &Object) -> Result<(), PgError> {
        async {
            let (begin, causation, claims) = tokio::join!(biased;
                conn.execute(&self.begin, &[]),
                async {
                    if let Some((statement, json)) = &self.causation {
                        conn.execute(statement, &[json]).await?;
                    }
                    Ok::<_, tokio_postgres::Error>(())
                },
                conn.execute_raw(&self.statement, self.params.iter()));
            begin.map_err(|error| map_pg_error(&error))?;
            causation.map_err(|error| map_pg_error(&error))?;
            claims.map_err(|error| map_pg_error(&error))?;
            Ok(())
        }
        .instrument(tracing::info_span!(
            "wamn.postgres.bind_claims",
            wamn.authority_class = self.class.as_str()
        ))
        .await
    }
}

impl WamnPostgres {
    /// `BEGIN` + claim/limit injection. The claims are injected by ONE fully
    /// bound statement ([`CLAIM_SQL`]) whose every value travels as a bind
    /// parameter — there is no interpolation path (R2/R16). `tenant` is always
    /// present; `schema`/`runner` bind NULL when absent (COALESCE-to-current
    /// preserves the server default / prior value — the S2/pgbench path is
    /// byte-unchanged), and `role`/`user_id` bind `''` when absent, the deny
    /// floor the compiled RLS predicates read. An absent `operation` also binds
    /// `''`. A run-owned transaction also
    /// appends the transactional `wamn.causation` emit (l5i9.12.2).
    ///
    /// Cost: `BEGIN` and the bound claim statement are pipelined (issued without
    /// an await between them; tokio-postgres preserves FIFO order so `BEGIN`
    /// opens the txn before the transaction-LOCAL `set_config`s apply), and the
    /// claim statement is `prepare_cached`, so the steady-state round-trip count
    /// on a pooled connection matches the pre-R2 single batch.
    #[expect(
        clippy::too_many_arguments,
        reason = "every claim this transaction binds is an independently trusted input"
    )]
    pub(in super::super) async fn begin_with_claims(
        &self,
        conn: &Object,
        class: AuthorityClass,
        read: bool,
        tenant: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&str>,
        operation: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<(), PgError> {
        self.prepare_claims(
            conn,
            class,
            read,
            tenant,
            schema,
            runner,
            role,
            user_id,
            operation,
            run,
            statement_timeout_ms,
        )
        .await?
        .bind(conn)
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "every claim this transaction binds is an independently trusted input"
    )]
    pub(in super::super) async fn prepare_claims(
        &self,
        conn: &Object,
        class: AuthorityClass,
        read: bool,
        tenant: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&str>,
        operation: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<BoundClaims, PgError> {
        // The tenant is still VALIDATED on the guest path even though it is no
        // longer bound: it selected the credential this connection was checked
        // out with, so a malformed one is a bug worth failing on.
        validate_claims(tenant, schema, runner, role, user_id, operation)?;
        let guest = class == AuthorityClass::GuestSql;
        // Prepare every statement before a caller polls the pipeline. A cache
        // miss must not let the application statement run ahead of BEGIN.
        let stmt = conn
            .prepare_cached(claim_sql(class))
            .await
            .map_err(|e| map_pg_error(&e))?;
        let mut params = vec![
            Some(statement_timeout_ms.to_string()),
            schema.map(str::to_owned),
            runner.map(str::to_owned),
            Some(role.unwrap_or_default().to_owned()),
            Some(user_id.unwrap_or_default().to_owned()),
            Some(operation.unwrap_or_default().to_owned()),
        ];
        if !guest {
            params.insert(0, Some(tenant.to_owned()));
        }
        // The isolation level is stated, not inherited from a session default:
        // the write log's retry reads the claim that its insert waited on, and
        // only a fresh READ COMMITTED snapshot sees it (`docs/plan/write-log.md`
        // 4.2). A guest has no way to name the level through `begin`; the
        // guest transaction of a read invocation (`read`) reads one snapshot.
        let begin = if read {
            BEGIN_READ_SNAPSHOT
        } else {
            BEGIN_READ_COMMITTED
        };
        let begin = conn
            .prepare_cached(begin)
            .await
            .map_err(|e| map_pg_error(&e))?;
        let causation = match run {
            Some(run) => Some(
                prepare_causation(conn, run)
                    .await
                    .map_err(|e| map_pg_error(&e))?,
            ),
            None => None,
        };
        Ok(BoundClaims {
            begin,
            causation,
            statement: stmt,
            params,
            class,
        })
    }

    /// Single statement in an implicit transaction: claims injected,
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
    /// component's single `wamn.project` claim.
    pub(in super::super) async fn one_shot_for_project(
        &self,
        component_id: &str,
        project: &str,
        sql: &str,
        params: &[SqlValue],
        want_rows: bool,
    ) -> Result<OneShotResult, PgError> {
        let tenant = self.require_tenant(component_id)?;
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let role = self.role_for(component_id);
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let run = self.current_run_for(component_id);
        let (conn, pp, authority) = self
            .checkout_workload(component_id, project, &tenant)
            .await?;
        if let Err(e) = self
            .begin_with_claims(
                &conn,
                authority,
                false,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                role.as_deref(),
                user_id.as_deref(),
                operation.as_deref(),
                run.as_ref(),
                pp.statement_timeout_ms,
            )
            .await
        {
            // Claim injection failed: connection state is unknown — destroy.
            self.destroy(conn);
            return Err(e);
        }
        let result = if want_rows {
            run_query(&conn, sql, params, pp.row_limit)
                .await
                .map(OneShotResult::Rows)
        } else {
            run_execute(&conn, sql, params)
                .await
                .map(OneShotResult::Count)
        };
        match result {
            Ok(v) => match finish_transaction(&conn, "COMMIT").await {
                Ok(()) => Ok(v),
                Err(e) => {
                    self.destroy(conn);
                    Err(map_pg_error(&e))
                }
            },
            Err(pg_err) => {
                // Statement failed; roll the implicit transaction back and
                // repool. If even ROLLBACK fails the connection is toast.
                if let Err(e) = conn.batch_execute("ROLLBACK").await {
                    tracing::warn!(error = %e, "rollback after failed statement also failed; destroying connection");
                    self.destroy(conn);
                }
                Err(pg_err)
            }
        }
    }

    /// Execute one admitted statement in an implicit claim-aware transaction.
    /// Contract drift rolls the transaction back before any mutation commits.
    pub(in super::super) async fn one_shot_statement(
        &self,
        component_id: &str,
        digest: &str,
        statement: &VerifiedStatement,
        binds: &[SqlValue],
    ) -> Result<RowSet, StatementError> {
        let project = self.project_for(component_id);
        let tenant = self
            .require_tenant(component_id)
            .map_err(StatementError::Postgres)?;
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let role = self.role_for(component_id);
        let user_id = self.user_id_for(component_id);
        refuse_unattributed_statement(statement, user_id.as_deref())
            .map_err(StatementError::Postgres)?;
        let operation = self.operation_for(component_id);
        let run = self.current_run_for(component_id);
        let (connection, policy, authority) = self
            .checkout_workload(component_id, &project, &tenant)
            .await
            .map_err(StatementError::Postgres)?;
        let connection = StatementConnectionGuard::new(connection, Arc::clone(&self.destroyed));
        // AUTOCOMMIT WHEN THE SERVER SAYS NO TRANSACTION IS NEEDED. PostgreSQL
        // classified this statement at generation time: it neither writes nor
        // takes a row lock, so BEGIN and COMMIT are ceremony around a read.
        // Measured cost of that ceremony: bind_claims 0.45-0.89 ms plus a
        // COMMIT of 2.1-3.8 ms, around a 0.6 ms statement
        // (measurement records in commits 494b5b8d5595 and 3659506bdeef).
        //
        // A read carrying a role claim keeps the transaction: a session-scoped
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
            let (settings, rows) = tokio::join!(
                biased;
                bind_session_settings(
                    connection.connection(), &policy, prepared.as_ref(), schema, runner,
                ),
                run_verified_query(
                    connection.connection(), digest, statement, binds, policy.row_limit,
                ),
            );
            settings.map_err(StatementError::Postgres)?;
            let rows = rows?;
            connection.repool();
            return Ok(rows);
        }

        // Prepare before polling the application query, including on a cold
        // connection. The biased join sends BEGIN and claims first.
        let claims = self
            .prepare_claims(
                connection.connection(),
                authority,
                false,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                role.as_deref(),
                user_id.as_deref(),
                operation.as_deref(),
                run.as_ref(),
                policy.statement_timeout_ms,
            )
            .await
            .map_err(StatementError::Postgres)?;
        let (claims, result) = tokio::join!(
            biased;
            claims.bind(connection.connection()),
            run_verified_query(
                connection.connection(),
                digest,
                statement,
                binds,
                policy.row_limit,
            ),
        );
        if let Err(error) = claims {
            // The statement rode the same flight into a transaction that never
            // opened, so its own error is a consequence, not the cause.
            if connection
                .connection()
                .batch_execute("ROLLBACK")
                .await
                .is_err()
            {
                tracing::warn!("rollback after failed claim binding also failed");
            }
            return Err(StatementError::Postgres(error));
        }
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
                if let Err(rollback_error) = connection.connection().batch_execute("ROLLBACK").await
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
