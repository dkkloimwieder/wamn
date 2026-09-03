//! Internal development-administrator adapter for the authoring commands.
//!
//! There is intentionally no CLI or public transport in this module. Item 5
//! owns retained client identity and client-facing authorization; this adapter
//! proves the shared typed command/query boundary first.
//!
//! It persists no authored document (wamn-0h0g.8.5.5). A draft is a client-side
//! file and the wiring document's content hash is its identity, so the only
//! authored state this adapter reaches is the immutable report a gate produced —
//! keyed by that same content hash (wamn-0h0g.8.5.6).

use anyhow::{Context as _, bail};
use serde_json::Value;
use tokio_postgres::{Client, NoTls, Transaction};

use wamn_control_provision::parse_control_authoring_url;
use wamn_schema_control::BareSchemaName;

/// Startup authority probe for the CONTROL database's author credential
/// (wamn-0h0g.8.18).
///
/// Every column must be true. The authority CLASS narrowed with wamn-0h0g.8.5.5:
/// the mutable-draft SELECT/INSERT/UPDATE leg is gone with `catalog.flow_drafts`
/// itself, leaving the append-only facts and the reservation and case-map
/// transitions. The principal is `wamn_control_author` and the relations live in
/// the control database.
///
/// Three legs of the project-residency probe are gone because the relations they
/// named do not exist in the control store: `catalog.connection_bindings`,
/// `catalog.connection_instances`, and `catalog.connection_generations` are
/// project-local after the plane split, and there is no `runs` relation to read
/// at all — project run observation stays with wamn-0h0g.8.5.
///
/// Two legs are new: `wamn_authority` is the third schema this credential may
/// use (for the tenant resolver alone), and the mapping relation that decides its
/// tenant must be completely unreachable, so an author cannot re-point itself.
///
/// Naming `wamn_authority.author_login_tenants` and
/// `catalog.deployment_attestations` here also makes the probe structurally
/// unable to pass against a project database: those relations are absent there,
/// so the statement errors and the process refuses instead of quietly authoring
/// into the wrong plane.
///
/// Params: `$1` run schema. wamn-0h0g.8.5.5 deleted the whole reservation-era
/// gate-report lineage and wamn-0h0g.8.5.6 put the surviving report row back, so
/// this credential may APPEND to exactly two relations — the command ledger and
/// `wamn_run.gate_reports` — and may rewrite neither. The run schema is still a
/// name the schema-level and blanket-exclusion legs resolve against, and now
/// also holds a relation those legs must find granted rather than excess.
const AUTHORING_ROLE_PROBE_SQL: &str = "\
WITH session_role AS ( \
    SELECT oid, rolsuper, rolcreatedb, rolcreaterole, rolreplication, rolbypassrls \
      FROM pg_catalog.pg_roles WHERE rolname = session_user \
), author_role AS ( \
    SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolreplication, rolbypassrls \
      FROM pg_catalog.pg_roles WHERE rolname = 'wamn_control_author' \
), allowed_mutation(schema_name, table_name, privilege) AS ( \
    VALUES ('catalog', 'authoring_command_audit', 'INSERT'), \
           ('wamn_run', 'gate_reports', 'INSERT') \
) \
SELECT current_user = session_user, \
       COALESCE(NOT session_role.rolsuper AND NOT session_role.rolcreatedb \
                AND NOT session_role.rolcreaterole AND NOT session_role.rolreplication \
                AND NOT session_role.rolbypassrls, false), \
       COALESCE(NOT author_role.rolcanlogin AND NOT author_role.rolsuper \
                AND NOT author_role.rolcreatedb AND NOT author_role.rolcreaterole \
                AND NOT author_role.rolreplication AND NOT author_role.rolbypassrls, false), \
       pg_catalog.pg_has_role(session_user, 'wamn_control_author', 'MEMBER'), \
       pg_catalog.pg_has_role(session_user, 'wamn_control_author', 'USAGE'), \
       NOT COALESCE((SELECT pg_catalog.pg_has_role(session_user, guest.oid, 'USAGE') \
                       FROM pg_catalog.pg_roles AS guest \
                      WHERE guest.rolname = 'wamn_app'), false), \
       NOT COALESCE((SELECT pg_catalog.pg_has_role(session_user, project_author.oid, 'USAGE') \
                       FROM pg_catalog.pg_roles AS project_author \
                      WHERE project_author.rolname = 'wamn_scenario_author'), false), \
       pg_catalog.has_schema_privilege(current_user, 'catalog', 'USAGE'), \
       pg_catalog.has_schema_privilege(current_user, $1, 'USAGE'), \
       pg_catalog.has_schema_privilege(current_user, 'wamn_authority', 'USAGE'), \
       NOT pg_catalog.has_schema_privilege(current_user, 'catalog', 'CREATE'), \
       NOT pg_catalog.has_schema_privilege(current_user, $1, 'CREATE'), \
       NOT pg_catalog.has_schema_privilege(current_user, 'wamn_authority', 'CREATE'), \
       pg_catalog.has_table_privilege(current_user, 'catalog.authoring_command_audit', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.authoring_command_audit', 'INSERT') \
         AND NOT pg_catalog.has_table_privilege( \
             current_user, 'catalog.authoring_command_audit', 'UPDATE') \
         AND NOT pg_catalog.has_table_privilege( \
             current_user, 'catalog.authoring_command_audit', 'DELETE'), \
       pg_catalog.has_table_privilege(current_user, 'wamn_run.gate_reports', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'wamn_run.gate_reports', 'INSERT') \
         AND NOT pg_catalog.has_table_privilege( \
             current_user, 'wamn_run.gate_reports', 'UPDATE') \
         AND NOT pg_catalog.has_table_privilege( \
             current_user, 'wamn_run.gate_reports', 'DELETE'), \
       pg_catalog.has_function_privilege( \
           current_user, 'wamn_authority.session_author_tenant()', 'EXECUTE'), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.unnest( \
               ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) \
                AS mapping_privilege(privilege) \
            WHERE pg_catalog.has_table_privilege( \
                current_user, 'wamn_authority.author_login_tenants', \
                mapping_privilege.privilege) \
               OR (mapping_privilege.privilege IN ('SELECT','INSERT','UPDATE','REFERENCES') \
                   AND pg_catalog.has_any_column_privilege( \
                       current_user, 'wamn_authority.author_login_tenants', \
                       mapping_privilege.privilege)) \
       ), \
       NOT pg_catalog.has_table_privilege( \
             current_user, 'catalog.deployment_attestations', 'SELECT') \
         AND NOT pg_catalog.has_table_privilege( \
             current_user, 'catalog.deployment_attestations', 'INSERT'), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_roles AS role \
            WHERE role.rolname NOT IN (session_user, 'wamn_control_author') \
              AND pg_catalog.pg_has_role(session_user, role.oid, 'MEMBER') \
       ), \
       NOT EXISTS ( \
           SELECT 1 \
             FROM pg_catalog.pg_class AS relation \
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
             CROSS JOIN pg_catalog.unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) \
                  AS candidate(privilege) \
            WHERE relation.relkind IN ('r','p') \
              AND namespace.nspname IN ('catalog', $1, 'wamn_authority') \
              AND (pg_catalog.has_table_privilege(current_user, relation.oid, candidate.privilege) \
                   OR (candidate.privilege IN ('INSERT','UPDATE','REFERENCES') \
                       AND pg_catalog.has_any_column_privilege( \
                           current_user, relation.oid, candidate.privilege))) \
              AND NOT EXISTS ( \
                  SELECT 1 FROM allowed_mutation AS allowed \
                   WHERE allowed.schema_name = namespace.nspname \
                     AND allowed.table_name = relation.relname \
                     AND allowed.privilege = candidate.privilege \
              ) \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_database \
            WHERE datname = pg_catalog.current_database() AND datdba = session_role.oid \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_namespace \
            WHERE nspname IN ('catalog', $1, 'wamn_authority') \
              AND nspowner = session_role.oid \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_class AS relation \
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
            WHERE namespace.nspname IN ('catalog', $1, 'wamn_authority') \
              AND relation.relkind IN ('r', 'p') \
              AND relation.relowner = session_role.oid \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_proc AS routine \
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace \
            WHERE namespace.nspname IN ('catalog', $1, 'wamn_authority') \
              AND routine.proowner = session_role.oid \
       ) \
  FROM session_role CROSS JOIN author_role";

/// The database-authoritative tenant binding, checked separately from the
/// authority probe so each refusal is attributable.
///
/// The mapping — not the caller-set `app.tenant` — decides which tenant this
/// login may reach. An unmapped login resolves NULL, so the comparison is NULL
/// and the process refuses.
const AUTHORING_TENANT_BINDING_SQL: &str = "SELECT wamn_authority.session_author_tenant() = $1";

/// `app.tenant` is retained as a CONSISTENCY ASSERTION ONLY (wamn-0h0g.8.18).
///
/// It still has to agree with the mapping for any row to be visible, because the
/// permissive tenant policy reads it, but it cannot widen anything: the
/// restrictive author policy is ANDed on top and resolves through `session_user`.
const AUTHORING_SCOPE_SQL: &str = "SELECT \
    pg_catalog.set_config('app.tenant', $1, false), \
    pg_catalog.set_config('search_path', $2, false)";

/// Capability token held only by the trusted host-side development adapter.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InternalDevAdmin {
    _private: (),
}

impl InternalDevAdmin {
    /// Construct the token at the internal process boundary.
    ///
    /// This does not authenticate a client and must not be exposed through a
    /// retained API surface before PLAN item 5 supplies that boundary.
    pub(crate) fn at_process_boundary() -> Self {
        Self { _private: () }
    }
}

/// The one management scope a control authoring connection is admitted for.
///
/// One management process serves exactly one `(org, project, environment)`
/// (wamn-0h0g.8.18), and that scope together with the database the connection
/// input names fully determines the A/B generation role it must authenticate as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlAuthoringScope {
    pub org: String,
    pub project: String,
    pub environment: String,
    /// The tenant this process authors for. The database mapping is
    /// authoritative; this is the value the mapping has to agree with, and
    /// disagreement refuses at startup rather than authoring for someone else.
    pub tenant_id: String,
    /// Schema carrying the reservation, case-map, and report relations. The
    /// qualified names are unchanged by the residency move: database residency,
    /// not a renamed schema, distinguishes the two stores.
    pub source_schema: String,
}

/// Trusted process-local adapter for the typed authoring commands and queries.
///
/// It owns a dedicated CONTROL-database author connection, a fixed tenant, and
/// a validated run-plane schema.
/// The private capability token is never returned to callers.
pub struct InternalAuthoringBackend {
    authority: InternalDevAdmin,
    client: Client,
    connection_task: tokio::task::JoinHandle<()>,
    tenant_id: Box<str>,
    source_schema: BareSchemaName,
}

impl InternalAuthoringBackend {
    /// Connect the sole authoring/report credential: a scoped A/B generation of
    /// `wamn_control_author` on the CONTROL database.
    ///
    /// Fails closed BEFORE ANY I/O when the connection input is absent or out of
    /// scope: [`parse_control_authoring_url`] is pure, and it runs before the
    /// tenant and schema validators and before `tokio_postgres::connect`. There is
    /// no project-URL fallback, no dual read, and no dual write — this is the one
    /// connection, and `WAMN_SYSTEM_URL` remains a separate identity-read
    /// connection the caller owns.
    pub async fn connect(
        control_authoring_database_url: &str,
        scope: &ControlAuthoringScope,
    ) -> anyhow::Result<Self> {
        let connection = parse_control_authoring_url(
            control_authoring_database_url,
            &scope.org,
            &scope.project,
            &scope.environment,
        )?;
        if !wamn_control_registry::identifiers::valid_tenant(&scope.tenant_id) {
            bail!("invalid fixed authoring tenant identity");
        }
        let source_schema = BareSchemaName::new(scope.source_schema.clone())
            .context("invalid fixed authoring run schema")?;
        tracing::info!(
            database = connection.database(),
            role = connection.role(),
            generation = connection.generation().as_str(),
            "control authoring credential accepted"
        );
        let (client, driver) = tokio_postgres::connect(control_authoring_database_url, NoTls)
            .await
            .context("connect dedicated control authoring database credential")?;
        let connection_task = tokio::spawn(async move {
            if let Err(error) = driver.await {
                tracing::error!(%error, "control authoring database connection failed");
            }
        });
        client
            .query_one(
                "SELECT pg_catalog.set_config('search_path', 'pg_catalog', false)",
                &[],
            )
            .await
            .context("pin trusted search path before authoring authority probe")?;
        let role_row = client
            .query_one(AUTHORING_ROLE_PROBE_SQL, &[&source_schema.as_str()])
            .await
            .context("verify effective dedicated control authoring authority")?;
        for index in 0..role_row.len() {
            if !role_row
                .try_get::<_, Option<bool>>(index)
                .context("decode authoring authority probe")?
                .unwrap_or(false)
            {
                connection_task.abort();
                bail!(
                    "database session is not an unprivileged, effectively authorized, author-only credential"
                );
            }
        }
        // Tenant authority is the owner-maintained mapping, never `app.tenant`.
        let bound = client
            .query_one(AUTHORING_TENANT_BINDING_SQL, &[&scope.tenant_id])
            .await
            .context("resolve the control author's mapped tenant")?
            .try_get::<_, Option<bool>>(0)
            .context("decode the control author's mapped tenant")?;
        if bound != Some(true) {
            connection_task.abort();
            bail!("control author login is not mapped to this process's fixed tenant");
        }
        let backend = Self {
            authority: InternalDevAdmin::at_process_boundary(),
            client,
            connection_task,
            tenant_id: scope.tenant_id.clone().into_boxed_str(),
            source_schema,
        };
        backend.scope().await?;
        Ok(backend)
    }

    fn require_tenant(&self, tenant_id: &str) -> anyhow::Result<()> {
        if tenant_id != self.tenant_id.as_ref() {
            bail!("authoring command tenant differs from the backend's fixed tenant");
        }
        Ok(())
    }

    async fn scope(&self) -> anyhow::Result<()> {
        self.client
            .query_one(
                AUTHORING_SCOPE_SQL,
                &[&self.tenant_id.as_ref(), &self.source_schema.as_str()],
            )
            .await
            .context("inject fixed authoring tenant and run schema")?;
        Ok(())
    }

    /// Begin one command transaction after reasserting the fixed tenant scope.
    ///
    /// The returned capability copy and transaction must stay together: the
    /// management boundary uses them to serialize retry identity, execute the
    /// command, and persist its exact outcome as one atomic unit.
    pub(crate) async fn begin_command_transaction(
        &mut self,
        tenant_id: &str,
    ) -> anyhow::Result<(InternalDevAdmin, Transaction<'_>)> {
        self.require_tenant(tenant_id)?;
        self.scope().await?;
        let authority = self.authority;
        let transaction = self
            .client
            .transaction()
            .await
            .context("begin authoring command transaction")?;
        Ok((authority, transaction))
    }
}

impl Drop for InternalAuthoringBackend {
    fn drop(&mut self) {
        self.connection_task.abort();
    }
}

/// Control-store projection of one test report.
///
/// wamn-0h0g.8.5.5 deleted the run-plane report lineage: `Pending` was
/// reachable ONLY while the reservation protocol stood, and the reservation is
/// gone. What survives is the two answers the wire contract still has —
/// `report-not-found` and one immutable finalized projection.
#[derive(Clone, Debug, PartialEq)]
pub enum GetReportResult {
    /// No report is visible in the backend's fixed tenant.
    NotFound,
    /// The immutable report exists.
    Finalized {
        validated_draft_id: String,
        passed: bool,
        summary: Value,
    },
}

/// Read one gate report by its key.
///
/// The tenant predicate is explicit as well as enforced by the relation's two
/// policies, on the same terms as every other statement this credential runs: a
/// query whose only scope is a session claim reads as unscoped.
///
/// Params: `$1` tenant, `$2` wiring hash.
const SELECT_GATE_REPORT_SQL: &str = "SELECT passed, summary \
    FROM wamn_run.gate_reports \
    WHERE tenant_id = $1 AND wiring_hash = $2";

impl InternalAuthoringBackend {
    /// Read one finalized report from the fixed control-store scope.
    ///
    /// `report_id` IS the wiring hash (wamn-0h0g.8.5.6) — the same collapse that
    /// deleted `catalog.wirings.gate_report_id`, applied to the read side. The
    /// gate verb writes one row per document it ACCEPTS, so `NotFound` is the
    /// truthful answer for a document that was never gated, for one whose gate
    /// refused it, and for a mistyped hash alike: in every case the store holds
    /// no report under that key.
    pub async fn get_report(
        &self,
        tenant_id: &str,
        report_id: &str,
    ) -> anyhow::Result<GetReportResult> {
        self.require_tenant(tenant_id)?;
        validate_identity(report_id, "report-id")?;
        let Some(row) = self
            .client
            .query_opt(SELECT_GATE_REPORT_SQL, &[&tenant_id, &report_id])
            .await
            .context("read one gate report")?
        else {
            return Ok(GetReportResult::NotFound);
        };
        Ok(GetReportResult::Finalized {
            validated_draft_id: report_id.to_owned(),
            passed: row.get(0),
            summary: row.get(1),
        })
    }
}

fn validate_identity(value: &str, name: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wamn-0h0g.8.18: the probe targets the CONTROL author, and structurally
    /// cannot pass against a project database or admit another plane's role.
    #[test]
    fn authoring_probe_targets_the_control_author_and_no_other_plane() {
        // The principal moved; the authority class did not.
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("rolname = 'wamn_control_author'"));
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("pg_catalog.pg_has_role(session_user, 'wamn_control_author', 'MEMBER')")
        );
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("role.rolname NOT IN (session_user, 'wamn_control_author')")
        );
        // The project plane's author role is never reused here, and the guest role
        // is denied. Both checks are existence-safe, because roles are
        // cluster-global and a control-only cluster need not carry either.
        for denied in ["'wamn_app'", "'wamn_scenario_author'"] {
            assert!(
                AUTHORING_ROLE_PROBE_SQL.contains(&format!("WHERE guest.rolname = {denied}"))
                    || AUTHORING_ROLE_PROBE_SQL
                        .contains(&format!("WHERE project_author.rolname = {denied}")),
                "the probe stopped denying {denied}"
            );
        }
        assert!(!AUTHORING_ROLE_PROBE_SQL.contains("pg_has_role(session_user, 'wamn_app'"));
        assert!(
            !AUTHORING_ROLE_PROBE_SQL.contains("pg_has_role(session_user, 'wamn_scenario_author'")
        );

        // Relations that only a PROJECT database has are gone: reading them would
        // have made the control credential impossible to admit.
        for project_local in [
            "catalog.connection_bindings",
            "catalog.connection_instances",
            "catalog.connection_generations",
            "'runs'",
            ".runs",
        ] {
            assert!(
                !AUTHORING_ROLE_PROBE_SQL.contains(project_local),
                "the control probe still reads project-local {project_local}"
            );
        }
        // Conversely, relations only a CONTROL database has are named, so the probe
        // errors rather than admitting a project connection.
        assert!(
            AUTHORING_ROLE_PROBE_SQL.contains("'wamn_authority.author_login_tenants'")
                && AUTHORING_ROLE_PROBE_SQL.contains("'catalog.deployment_attestations'")
        );
        // Tenant authority is not self-service: no privilege at all on the mapping.
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("mapping_privilege.privilege"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains(
            "ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']"
        ));
        // The resolver is reachable; the mapped tenant is checked separately so a
        // missing mapping and a missing privilege are distinguishable failures.
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("'wamn_authority.session_author_tenant()', 'EXECUTE'")
        );
        assert_eq!(
            AUTHORING_TENANT_BINDING_SQL,
            "SELECT wamn_authority.session_author_tenant() = $1"
        );
        assert!(!AUTHORING_TENANT_BINDING_SQL.contains("app.tenant"));
        // The publisher relation is not readable, let alone writable. Compared on
        // whitespace-normalized text so line wrapping is not load-bearing.
        let probe = AUTHORING_ROLE_PROBE_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for withheld in [
            "'catalog.deployment_attestations', 'SELECT'",
            "'catalog.deployment_attestations', 'INSERT'",
        ] {
            assert!(
                probe.contains(&format!(
                    "NOT pg_catalog.has_table_privilege( current_user, {withheld})"
                )),
                "the probe stopped denying {withheld}"
            );
        }
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("pg_catalog.pg_database"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("pg_catalog.pg_namespace"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("relation.relowner = session_role.oid"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("routine.proowner = session_role.oid"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("pg_catalog.has_any_column_privilege"));
        // wamn-0h0g.8.5.5: the draft-safe connection-grant relation is deleted,
        // so the probe may not name it at all. A surviving arm would query a
        // relation that does not exist and refuse every startup.
        assert!(!AUTHORING_ROLE_PROBE_SQL.contains("draft_safe_connection_grants"));
        // wamn-0h0g.8.5.5: the mutable draft store is deleted too, so the probe
        // may not name it either -- neither as a privilege leg nor as an
        // allowed mutation. A surviving leg would query a missing relation and
        // refuse every startup.
        assert!(!AUTHORING_ROLE_PROBE_SQL.contains("flow_drafts"));
        assert!(!AUTHORING_ROLE_PROBE_SQL.contains("authoring_test_sets"));
        // wamn-0h0g.8.5.5: the whole run-plane gate-report lineage is deleted,
        // so the probe may not name any of its three relations -- not as a
        // privilege leg, not as an allowed mutation, and not as a parameter. A
        // surviving arm would query a relation that does not exist and refuse
        // every startup, which is the same failure mode the draft store had.
        for retired in [
            "authoring_test_run_reservations",
            "authoring_test_case_runs",
            "authoring_test_reports",
        ] {
            assert!(!AUTHORING_ROLE_PROBE_SQL.contains(retired), "{retired}");
        }
        // Only the run schema remains: the retired legs took $2 through $4 with
        // them, and the retired validator never added a fifth.
        for parameter in ["$2", "$3", "$4", "$5"] {
            assert!(!AUTHORING_ROLE_PROBE_SQL.contains(parameter), "{parameter}");
        }
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("$1"));
        assert!(!AUTHORING_ROLE_PROBE_SQL.contains("run_mutation"));
        // The command ledger is the ONE mutation this credential retains.
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("VALUES ('catalog', 'authoring_command_audit', 'INSERT')")
        );
        assert!(
            !AUTHORING_ROLE_PROBE_SQL
                .to_ascii_uppercase()
                .contains("SET ROLE")
        );
    }
}
