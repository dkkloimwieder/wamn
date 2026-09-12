use std::sync::Arc;

use deadpool_postgres::Object;
use tokio_postgres::types::ToSql;
use tracing::Instrument as _;

use wamn_event_wire::Causation;
use wamn_run_state::AuthorityClass;

use super::super::resources::{
    StatementConnectionGuard, run_execute, run_query, run_verified_query,
};
use super::super::statements::VerifiedStatement;
use super::super::types::map_pg_error;
use super::super::{PgError, RowSet, SqlValue, StatementError};
use super::{OneShotResult, WamnPostgres, validate_claims};

/// The transactional `wamn.causation` logical-message emit appended to a
/// run-owned transaction's BEGIN batch (l5i9.12.2). The [`Causation`] is
/// serialized canonically (`{"run":..,"root":..,"depth":..}` — the reader
/// deserializes with `deny_unknown_fields`) and SQL-escaped (single quotes
/// doubled) for safe literal embedding in the simple-query batch, which takes
/// no bind params. `transactional = true` so the message rides the txn's commit
/// at its own LSN; the reader (l5i9.12.1) buffers the whole txn and stamps this
/// onto every row event regardless of frame order.
pub(super) fn causation_emit_sql(c: &Causation) -> String {
    let json = serde_json::to_string(c).expect("Causation serializes to JSON");
    let literal = wamn_pg_core::quote_literal(&json);
    format!(" SELECT pg_logical_emit_message(true, 'wamn.causation', {literal});")
}

/// The fully-bound claim statement run inside the plugin-managed transaction
/// (R2/R16). Every claim value travels as a bind parameter (`$1..$6`) — there is
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
/// - `$5` `app.role` / `$6` `app.user_id` — the per-role / per-user RLS claims
///   the compiled policies key on (wamn-0h0g.23.1). Bound UNCONDITIONALLY, not
///   COALESCEd to the current value like `$3`/`$4`: an absent claim binds `''`,
///   which is exactly the deny floor
///   `COALESCE(current_setting('app.role', true), '')` and
///   `NULLIF(current_setting('app.user_id', true), '')::uuid` use in the static
///   application-schema RLS contract. Re-asserting whatever the
///   pooled connection currently carries would let a session-level value survive
///   into the next component's transaction, turning a shared connection into a
///   role escalation; binding the floor cannot.
///
/// The `wamn.causation` emit (l5i9.12.2) is NOT part of this statement — it is a
/// separate, already-escaped simple-query emit appended by [`begin_with_claims`]
/// only for a run-owned transaction.
pub(super) const CLAIM_SQL: &str = "SELECT \
     set_config('app.tenant', $1, true), \
     set_config('statement_timeout', $2, true), \
     set_config('search_path', COALESCE($3, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($4, current_setting('app.runner', true)), true), \
     set_config('app.role', $5, true), \
     set_config('app.user_id', $6, true)";

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
/// and explicitly outside `wamn-0h0g.22.6`'s scope.
/// The SESSION-scoped settings an autocommit read still needs.
///
/// `search_path` and `statement_timeout` are not claims -- they are how the
/// connection resolves this package's unqualified relations and how long it may
/// run. A read outside a transaction cannot take them transaction-locally, and
/// without `search_path` every generated statement fails to resolve its own
/// tables. They are POOL-UNIFORM: the pool is keyed by class, project and
/// tenant, so every borrower of this connection wants the same two values.
///
/// `app.role` and `app.user_id` are deliberately ABSENT. Those are per-caller
/// claims, and a session-scoped claim would outlive the request and reach the
/// next borrower of the pooled connection -- the exact leak the claim model
/// exists to prevent. A request carrying either one takes the transactional
/// path instead.
const GUEST_AUTOCOMMIT_SETTINGS_SQL: &str = "SELECT \
     set_config('statement_timeout', $1, false), \
     set_config('search_path', COALESCE($2, current_setting('search_path')), false), \
     set_config('app.runner', COALESCE($3, current_setting('app.runner', true)), false)";

const GUEST_CLAIM_SQL: &str = "SELECT \
     set_config('statement_timeout', $1, true), \
     set_config('search_path', COALESCE($2, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($3, current_setting('app.runner', true)), true), \
     set_config('app.role', $4, true), \
     set_config('app.user_id', $5, true)";

/// The bound claim statement one authority class binds. ONE function, so the
/// pipelined path's warm-up and the transaction that follows it cannot disagree
/// about which cache entry they mean (wamn-0h0g.17.33).
const fn claim_sql(class: AuthorityClass) -> &'static str {
    match class {
        AuthorityClass::GuestSql => GUEST_CLAIM_SQL,
        _ => CLAIM_SQL,
    }
}

impl WamnPostgres {
    /// `BEGIN` + claim/limit injection. The claims are injected by ONE fully
    /// bound statement ([`CLAIM_SQL`]) whose every value travels as a bind
    /// parameter — there is no interpolation path (R2/R16). `tenant` is always
    /// present; `schema`/`runner` bind NULL when absent (COALESCE-to-current
    /// preserves the server default / prior value — the S2/pgbench path is
    /// byte-unchanged), and `role`/`user_id` bind `''` when absent, the deny
    /// floor the compiled RLS predicates read. A run-owned transaction also
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
        tenant: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<(), PgError> {
        // The tenant is still VALIDATED on the guest path even though it is no
        // longer bound: it selected the credential this connection was checked
        // out with, so a malformed one is a bug worth failing on.
        validate_claims(tenant, schema, runner, role, user_id)?;
        let guest = class == AuthorityClass::GuestSql;
        // A CACHE HIT SENDS NOTHING AND DOES NOT YIELD (deadpool's cell is
        // checked before the init future is ever awaited), so this call is the
        // whole reason the flight below can be interleaved with a caller's own
        // statement. A MISS is a Parse and a full round trip, and whatever is
        // polled during that await reaches the server FIRST -- so a caller that
        // pipelines must run [`WamnPostgres::warm_claim_statement`] before it
        // starts the other half (wamn-0h0g.17.33).
        let stmt = conn
            .prepare_cached(claim_sql(class))
            .await
            .map_err(|e| map_pg_error(&e))?;
        // statement_timeout binds as TEXT (a bare-integer string = ms).
        let timeout = statement_timeout_ms.to_string();
        // An absent role / user id binds the empty claim, not NULL: `''` is the
        // value the compiled policies' COALESCE / NULLIF floors deny on.
        let role = role.unwrap_or_default();
        let user_id = user_id.unwrap_or_default();
        let platform_params: [&(dyn ToSql + Sync); 6] =
            [&tenant, &timeout, &schema, &runner, &role, &user_id];
        let guest_params: [&(dyn ToSql + Sync); 5] = [&timeout, &schema, &runner, &role, &user_id];
        let params: &[&(dyn ToSql + Sync)] = if guest {
            &guest_params
        } else {
            &platform_params
        };
        // l5i9.12.2: stamp the run's causation onto this txn, IN THE BEGIN
        // BATCH. The TRANSACTIONAL emit rides the commit; a rolled-back txn
        // emits nothing and the reader (l5i9.12.1) stitches it onto the txn's
        // row events regardless of frame order. It carries no bind params, so
        // the already-escaped simple-query emit is unchanged by R2.
        //
        // RIDING BEGIN RATHER THAN FOLLOWING IT is what keeps the claim half of
        // a run-owned request to ONE flight: as a separate `batch_execute` it
        // could not be sent until BEGIN's own reply came back, and it would then
        // be the only message this function still had outstanding when a
        // pipelined caller's statement failed -- so an aborted transaction
        // surfaced here as a claim error and misattributed the cause.
        let begin_sql = match run {
            Some(run) => format!("BEGIN;{}", causation_emit_sql(run)),
            None => "BEGIN".to_string(),
        };
        // ONE FLIGHT. `batch_execute` and `execute` each enqueue their whole
        // message batch synchronously on their FIRST poll, and `biased;` pins
        // that poll order, so BEGIN is on the wire ahead of the bound claim
        // statement and tokio-postgres's FIFO ordering does the rest: the txn is
        // open before the transaction-LOCAL `set_config`s run. Nothing here
        // awaits before those two sends, which is the property a pipelining
        // caller depends on.
        //
        // Claim binding is a full server round trip on every request. It sat
        // inside wamn.postgres with no span of its own, so it was
        // indistinguishable from pool checkout and statement time.
        async {
            let (begin, claims) = tokio::join!(
                biased;
                conn.batch_execute(&begin_sql),
                conn.execute(&stmt, params),
            );
            begin.map_err(|e| map_pg_error(&e))?;
            claims.map_err(|e| map_pg_error(&e))?;
            Ok::<(), PgError>(())
        }
        .instrument(tracing::info_span!(
            "wamn.postgres.bind_claims",
            wamn.authority_class = class.as_str(),
        ))
        .await
    }

    /// Parse the bound claim statement on `conn` if this connection has not
    /// parsed it yet, so that the [`begin_with_claims`] that follows reaches its
    /// `BEGIN` inside its own first poll.
    ///
    /// This exists ONLY for the pipelined path (wamn-0h0g.17.33). A caller that
    /// simply awaits `begin_with_claims` does not need it -- the `prepare_cached`
    /// in there is the same call and the same cache entry.
    ///
    /// [`begin_with_claims`]: WamnPostgres::begin_with_claims
    async fn warm_claim_statement(
        &self,
        conn: &Object,
        class: AuthorityClass,
    ) -> Result<(), PgError> {
        conn.prepare_cached(claim_sql(class))
            .await
            .map(drop)
            .map_err(|e| map_pg_error(&e))
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
        let run = self.current_run_for(component_id);
        let (conn, pp, authority) = self
            .checkout_workload(component_id, project, &tenant)
            .await?;
        if let Err(e) = self
            .begin_with_claims(
                &conn,
                authority,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                role.as_deref(),
                user_id.as_deref(),
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
            Ok(v) => match conn.batch_execute("COMMIT").await {
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
        // A read carrying a per-caller claim keeps the transaction: a
        // session-scoped app.role or app.user_id would outlive the request and
        // reach the next borrower of this pooled connection.
        if !statement.transactional && role.is_none() && user_id.is_none() {
            let timeout = policy.statement_timeout_ms.to_string();
            // The settings must be APPLIED before the statement that depends on
            // search_path. They cannot ride the statement's flight: each side is
            // a prepare followed by an execute, and interleaving two such
            // futures does not order the two EXECUTES -- only the sends. So this
            // is two flights, not one, and it still removes the COMMIT.
            //
            // wamn-0h0g.17.18 moves these to connection setup, where they belong:
            // they are pool-uniform, so paying for them per request is waste.
            async {
                let prepared = connection
                    .connection()
                    .prepare_cached(GUEST_AUTOCOMMIT_SETTINGS_SQL)
                    .await
                    .map_err(|error| map_pg_error(&error))?;
                connection
                    .connection()
                    .execute(
                        &prepared,
                        &[&timeout, &schema.as_deref(), &runner.as_deref()],
                    )
                    .await
                    .map_err(|error| map_pg_error(&error))
            }
            .instrument(tracing::info_span!("wamn.postgres.session_settings"))
            .await
            .map_err(StatementError::Postgres)?;
            let rows = run_verified_query(
                connection.connection(),
                digest,
                statement,
                binds,
                policy.row_limit,
            )
            .await?;
            connection.repool();
            return Ok(rows);
        }

        // THE CLAIMS LAND BEFORE ANYTHING ELSE TOUCHES THE CONNECTION, AND THEY
        // STILL TRAVEL IN THE STATEMENT'S FLIGHT.
        //
        // The claim transaction and the statement are issued without an await
        // between them, on the reasoning that tokio-postgres preserves FIFO
        // order per connection. That reasoning was right about FIFO and wrong
        // about what had been SENT: `begin_with_claims` OPENED by awaiting its
        // own `prepare_cached`, and on a newly created connection that Parse is
        // a full round trip. The statement half, polled during that await, sent
        // its Parse first -- measured in the Receiving journey cluster with
        // log_statement=all and reproducible by restarting the hosts
        // (wamn-0h0g.15.137.15): the guest statement parsed before BEGIN and
        // failed with `relation "purchase_order" does not exist`, carrying
        // neither the `search_path` nor the `app.role` / `app.user_id` the
        // claims install. Awaiting the claims outright fixed the order and cost
        // the round trip the flight saved -- bind_claims at 0.45-0.89 ms plus a
        // wakeup, against a 0.6 ms statement (measurement record in commit 494b5b8d5595).
        //
        // Parsing the claim statement FIRST deletes the await that let it
        // happen, and deletes it for a COLD connection too, which is why this
        // needs no knowledge of what the statement cache holds.
        // `begin_with_claims` then reaches its `BEGIN` inside its own first
        // poll, so `BEGIN` and the bound claim statement are the first two
        // messages this request enqueues whatever this connection has cached. A
        // cold statement's Parse is enqueued behind them and the server, reading
        // its socket in order, runs it INSIDE the transaction with the claims
        // already applied. `biased;` pins that poll order instead of leaving it
        // to `join!`'s rotation (wamn-0h0g.17.33).
        //
        // Tested by `live_a_cold_connection_parses_inside_the_claim_transaction`,
        // which fails against either the pre-fix shape or a swapped `join!`.
        if let Err(error) = self
            .warm_claim_statement(connection.connection(), authority)
            .await
        {
            // Nothing is open yet -- the guard destroys the connection.
            return Err(StatementError::Postgres(error));
        }
        let (claims, result) = tokio::join!(
            biased;
            self.begin_with_claims(
                connection.connection(),
                authority,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                role.as_deref(),
                user_id.as_deref(),
                run.as_ref(),
                policy.statement_timeout_ms,
            ),
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
            Ok(rows) => match async { connection.connection().batch_execute("COMMIT").await }
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
