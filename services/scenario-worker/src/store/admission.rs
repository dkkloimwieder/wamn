//! The gate verb, and the PROJECT-database reads it judges a candidate from.
//!
//! Residency (wamn-0h0g.8.5.4): everything here runs on the SECOND connection —
//! a scoped `wamn_management_admitter` generation on this environment's PROJECT
//! database (wamn-0h0g.8.5.3 landed the input).
//!
//! # What wamn-0h0g.8.5.5 left standing
//!
//! A gate is a JUDGMENT ABOUT A DOCUMENT, not an execution of it (ratified spec
//! §5.1). The sequential per-ordinal reserve→admit→poll→evaluate→finalize loop
//! was the resumption protocol for effectful cases, and the effect-free clause
//! deleted the thing it remembered; the control-database half it wrote to went
//! with it, under the owner ruling of 2026-08-25 that a relation whose writer,
//! reader and keying all die does not survive. So the gate verb lives HERE now,
//! on the one connection it still needs, and it opens no transaction at all.
//!
//! The surface is deliberately narrow — resolve one candidate, read the two
//! postures that can refuse it — because that is the whole of what the admitter
//! credential is granted (`MANAGEMENT_ADMITTER_*` in
//! `crates/control/provision/src/sql.rs`). A column absent from those lists is
//! DENIED, not merely unmentioned, so a wider read here would fail closed at
//! runtime rather than compile-time.

use anyhow::{Context as _, bail};
use serde_json::Value;
use tokio_postgres::{Client, NoTls};

use wamn_authoring_model::GateRefusal;
use wamn_control_provision::{
    MANAGEMENT_ADMITTER_ROLE, ManagementAdmissionConnection, parse_management_admission_url, sql,
};
use wamn_execution_contract::{TestSetCase, validate_cases};
use wamn_runtime::plugins::wamn_postgres::{
    AclExpectation, AclTarget, AmbientCredentialState, CredentialExactnessProbe,
    CredentialProbeError, ExpectedCredentialIdentity, MembershipExpectation, MembershipMode,
    credential_exactness_probe, explicit_credential_source,
};

/// Inject the tenant every project-plane row policy resolves against.
///
/// Every relation this connection reads — `catalog.wirings`,
/// `catalog.component_library`, and the connection records — FORCES row-level
/// security keyed on `NULLIF(current_setting('app.tenant', true), '')`. One
/// session-level injection therefore scopes them all identically; an uninjected
/// session sees zero rows rather than another tenant's.
const ADMISSION_SCOPE_SQL: &str = "SELECT \
    pg_catalog.set_config('app.tenant', $1, false), \
    pg_catalog.set_config('search_path', 'pg_catalog', false)";

/// Resolve the candidate wiring a test-set command names.
///
/// Keyed on `wiring_hash` alone: the owner ruled that `validated_draft_id` IS
/// the wiring hash, so the command carries the whole identity and no
/// cross-database mapping exists or is needed. Every remaining admission
/// parameter — `catalog_id`, `wiring_id`, `version`, `gated_catalog_version` —
/// is a column of the row the hash selects, which is why the admitter needs no
/// `catalog.catalog_heads` grant to find them.
const SELECT_CANDIDATE_BY_HASH_SQL: &str = "SELECT catalog_id, wiring_id, version, \
        gated_catalog_version, graph_json \
    FROM catalog.wirings \
    WHERE tenant_id = $1 AND wiring_hash = $2 \
    ORDER BY catalog_id, wiring_id, version";

/// Name the components a gate case would reach whose admitted effects
/// projection is NOT empty.
///
/// The constitutional clause (wamn-0h0g.8.5.5, ratified spec section 5.1): a
/// gate is a JUDGMENT ABOUT A DOCUMENT, not an execution of it. Effects belong
/// to admitted runs under run identity, and a report keyed by content hash must
/// be reproducible from the document alone or that identity is a lie.
///
/// Enforcement is the effect-posture fact `wamn-0h0g.21.9` mints AT ADMISSION:
/// `catalog.component_library.effects` is the validator's derived projection of
/// a component's imports onto the authority packages that leave the host, and a
/// projection no validator derived is already refused at publication and on the
/// serving path. This is a THIRD READER of that same fact, not a new mechanism —
/// it derives nothing and asserts nothing of its own, it only reads the stored
/// projection and refuses a candidate that reaches a non-empty one.
///
/// The join is the candidate's `nodes` object onto the library at the candidate's
/// own applied catalog version, exactly as the store-alias diagnostic below
/// resolves it, so a gate and a run agree on which components a document reaches.
/// A node naming no library row contributes nothing here: an unresolvable
/// component is the admission statement's `candidate-definition-invalid`, a
/// different and already-typed refusal.
///
/// Params: `$1` tenant, `$2` catalog id, `$3` catalog version, `$4` nodes.
const SELECT_EFFECTFUL_COMPONENTS_SQL: &str = "WITH node AS ( \
        SELECT entry.value ->> 'component' AS component, \
               entry.value ->> 'interface-version' AS interface_version, \
               entry.value ->> 'operation' AS operation \
          FROM jsonb_each($4::jsonb) AS entry \
    ) \
    SELECT DISTINCT library.component \
      FROM node JOIN catalog.component_library AS library \
        ON library.tenant_id = $1 AND library.catalog_id = $2 \
       AND library.catalog_version = $3 \
       AND library.component = node.component \
       AND library.interface_version = node.interface_version \
       AND library.operation = node.operation \
     WHERE jsonb_array_length(library.effects) > 0 \
     ORDER BY 1";

/// Name the store aliases the candidate requires and this environment cannot
/// resolve.
///
/// This mirrors the `requirements` / `resolved_requirements` legs of the
/// admission statement, including instance lifecycle and active generation, so
/// a refusal names exactly the aliases whose absence produced
/// [`AdmissionResult::BindingWorldUnavailable`]. It is a diagnostic read only —
/// the admission statement, not this query, decides whether a run is admitted.
const SELECT_UNRESOLVED_STORE_ALIASES_SQL: &str = "WITH node AS ( \
        SELECT entry.value ->> 'component' AS component, \
               entry.value ->> 'interface-version' AS interface_version, \
               entry.value ->> 'operation' AS operation \
          FROM jsonb_each($4::jsonb) AS entry \
    ), component AS ( \
        SELECT DISTINCT library.component_digest \
          FROM node JOIN catalog.component_library AS library \
            ON library.tenant_id = $1 AND library.catalog_id = $2 \
           AND library.catalog_version = $3 \
           AND library.component = node.component \
           AND library.interface_version = node.interface_version \
           AND library.operation = node.operation \
    ), requirement AS ( \
        SELECT required.component_digest, required.store_alias \
          FROM component JOIN catalog.connection_requirements AS required \
            ON required.tenant_id = $1 AND required.artifact_hash IS NULL \
           AND required.requirement_name IS NULL \
           AND required.component_digest = component.component_digest \
    ) \
    SELECT DISTINCT requirement.store_alias \
      FROM requirement \
      LEFT JOIN catalog.connection_bindings AS binding \
        ON binding.tenant_id = $1 AND binding.catalog_id = $2 \
       AND binding.catalog_version = $3 AND binding.artifact_hash IS NULL \
       AND binding.requirement_name IS NULL \
       AND binding.component_digest = requirement.component_digest \
       AND binding.store_alias = requirement.store_alias \
       AND binding.environment = $5 AND binding.binding_status = 'active' \
       AND binding.validation_status = 'valid' \
      LEFT JOIN catalog.connection_instances AS instance \
        ON instance.tenant_id = binding.tenant_id \
       AND instance.environment = binding.environment \
       AND instance.instance_id = binding.instance_id \
       AND instance.lifecycle_status = 'enabled' \
       AND instance.active_generation IS NOT NULL \
      LEFT JOIN catalog.connection_generations AS generation \
        ON generation.tenant_id = instance.tenant_id \
       AND generation.environment = instance.environment \
       AND generation.instance_id = instance.instance_id \
       AND generation.generation = instance.active_generation \
     WHERE generation.generation IS NULL \
     ORDER BY 1";

/// The exact candidate row one test-set command selects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateWiring {
    pub catalog_id: String,
    /// The applied catalog version this definition was gated against. It is the
    /// admission statement's `expected_catalog_version` BY CONSTRUCTION: the
    /// candidate CTE joins `gated_catalog_version = expected_catalog_version`,
    /// so any other value selects no candidate at all.
    pub catalog_version: i32,
    pub wiring_id: String,
    pub wiring_version: i32,
    pub wiring_hash: String,
    /// The candidate's own `cases` array, riding `graph_json`.
    pub cases: Vec<TestSetCase>,
    /// The candidate's `nodes` object, retained to resolve the components it
    /// reaches: the effect posture that decides whether it may be gated at all,
    /// and the aliases that diagnose an unresolvable binding world.
    nodes: Value,
}

impl CandidateWiring {
    /// The `nodes` object as a `jsonb`-safe value.
    ///
    /// A candidate whose graph carries no object here reaches no component, and
    /// `jsonb_each` requires an object rather than a null.
    fn nodes_object(&self) -> Value {
        if self.nodes.is_object() {
            self.nodes.clone()
        } else {
            Value::Object(serde_json::Map::new())
        }
    }
}

/// One running management surface's project-database admission connection.
pub struct AdmissionSurface {
    client: Client,
    connection_task: tokio::task::JoinHandle<()>,
    tenant_id: Box<str>,
}

impl Drop for AdmissionSurface {
    fn drop(&mut self) {
        self.connection_task.abort();
    }
}

impl AdmissionSurface {
    /// Open the project-database admission credential for one fixed scope.
    ///
    /// Fails closed BEFORE ANY I/O when the input is absent or out of scope, on
    /// the same terms as the control-authoring connection: the parse is pure and
    /// runs first. `serve` already parsed the same value at startup; re-parsing
    /// here holds an in-process caller that never goes through `serve` to the
    /// identical gate.
    ///
    /// The parse is only half of it (wamn-0h0g.22.10). A URL is a CLAIM about who
    /// will connect, and no pure function can check it against the session the
    /// server actually opened, so this boundary then asks the server itself:
    /// [`admission_credential_probe`] is applied to the new connection BEFORE the
    /// tenant scope is injected or a single admission read runs.
    pub async fn connect(
        management_admission_database_url: &str,
        org: &str,
        project: &str,
        environment: &str,
        tenant_id: &str,
    ) -> anyhow::Result<Self> {
        let connection = parse_management_admission_url(
            management_admission_database_url,
            org,
            project,
            environment,
        )?;
        if !wamn_control_registry::identifiers::valid_tenant(tenant_id) {
            bail!("invalid fixed admission tenant identity");
        }
        let probe =
            admission_credential_probe(management_admission_database_url, &connection, tenant_id)
                .map_err(|error| {
                anyhow::anyhow!("management admission credential source refused: {error}")
            })?;
        tracing::info!(
            database = connection.database(),
            role = connection.role(),
            generation = connection.generation().as_str(),
            "management admission credential accepted"
        );
        let (client, driver) = probe
            .connection_config()
            .connect(NoTls)
            .await
            .context("connect dedicated project admission database credential")?;
        let connection_task = tokio::spawn(async move {
            if let Err(error) = driver.await {
                tracing::error!(%error, "project admission database connection failed");
            }
        });
        let surface = Self {
            client,
            connection_task,
            tenant_id: tenant_id.into(),
        };
        // Held BEFORE the scope injection and before any admission read: a
        // session the server does not agree is this generation never reaches a
        // statement of ours at all. `Drop` aborts the driver task on refusal.
        probe.probe_pooled(&surface.client).await.map_err(|error| {
            // The refusal carries a predicate and a kind, never credential
            // material or server detail.
            anyhow::anyhow!("management admission credential exactness refused: {error}")
        })?;
        surface.scope().await?;
        Ok(surface)
    }

    async fn scope(&self) -> anyhow::Result<()> {
        self.client
            .query_one(ADMISSION_SCOPE_SQL, &[&self.tenant_id.as_ref()])
            .await
            .context("inject fixed admission tenant scope")?;
        Ok(())
    }

    /// Resolve the one candidate a wiring hash names, or `None`.
    pub async fn candidate_by_hash(
        &self,
        wiring_hash: &str,
    ) -> anyhow::Result<Option<CandidateWiring>> {
        let rows = self
            .client
            .query(
                SELECT_CANDIDATE_BY_HASH_SQL,
                &[&self.tenant_id.as_ref(), &wiring_hash],
            )
            .await
            .context("resolve the candidate wiring by hash")?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        if rows.len() != 1 {
            bail!("one wiring hash selected {} candidate rows", rows.len());
        }
        let graph: Value = row.get(4);
        Ok(Some(CandidateWiring {
            catalog_id: row.get(0),
            catalog_version: row.get(3),
            wiring_id: row.get(1),
            wiring_version: row.get(2),
            wiring_hash: wiring_hash.to_owned(),
            cases: candidate_cases(&graph)?,
            nodes: graph.get("nodes").cloned().unwrap_or(Value::Null),
        }))
    }

    /// Name the effectful components this candidate reaches, for one refusal.
    ///
    /// Empty means the candidate is gateable: every component it reaches carries
    /// the empty effects projection, which is the POSITIVE fact the validator
    /// derived rather than the absence of one.
    pub async fn effectful_components(
        &self,
        candidate: &CandidateWiring,
    ) -> anyhow::Result<Vec<String>> {
        let rows = self
            .client
            .query(
                SELECT_EFFECTFUL_COMPONENTS_SQL,
                &[
                    &self.tenant_id.as_ref(),
                    &candidate.catalog_id,
                    &candidate.catalog_version,
                    &candidate.nodes_object(),
                ],
            )
            .await
            .context("name the candidate's effectful components")?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Name the candidate's unresolvable store aliases, for one refusal.
    pub async fn unresolved_store_aliases(
        &self,
        candidate: &CandidateWiring,
        environment: &str,
    ) -> anyhow::Result<Vec<String>> {
        let rows = self
            .client
            .query(
                SELECT_UNRESOLVED_STORE_ALIASES_SQL,
                &[
                    &self.tenant_id.as_ref(),
                    &candidate.catalog_id,
                    &candidate.catalog_version,
                    &candidate.nodes_object(),
                    &environment,
                ],
            )
            .await
            .context("name the candidate's unresolvable store aliases")?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }
}

/// Bind the parsed admission input to the exact facts the server must report.
///
/// [`parse_management_admission_url`] proves everything a PURE function can:
/// the input exists, names one database, and authenticates as one of this
/// `(org, project, environment)`'s two generation roles. What it cannot prove is
/// that the SERVER agrees — that is a fact about the opened session, not about
/// the input — so `current_user`, `current_database`, the tenant binding, the
/// stable ACL membership and the granted surface are asserted here instead of
/// assumed (wamn-0h0g.22.10).
///
/// The probe machinery is `wamn_runtime`'s and is consumed READ-ONLY: this is a
/// second caller beside the pooled runtime credential, and it derives no
/// predicate of its own.
///
/// `AmbientCredentialState::Absent` is asserted, not assumed:
/// `ManagementServeArgs::management_admission_database_url` deliberately carries
/// no `default_value` and no project-URL fallback, so the URL reaching here is
/// the one named explicit source. If a second source is ever reintroduced, this
/// refuses.
fn admission_credential_probe(
    management_admission_database_url: &str,
    connection: &ManagementAdmissionConnection,
    tenant_id: &str,
) -> Result<CredentialExactnessProbe, CredentialProbeError> {
    let source = explicit_credential_source(
        management_admission_database_url,
        tenant_id,
        AmbientCredentialState::Absent,
    )?;
    let mut acl = vec![AclExpectation::new(
        AclTarget::Schema("catalog".into()),
        "USAGE",
        true,
    )];
    // Driven from the provisioner's OWN list rather than a second copy of it, so
    // the readable surface this boundary asserts cannot drift from the one
    // `grant_management_admitter_surface_sql` grants.
    for relation in sql::MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        acl.push(AclExpectation::new(
            AclTarget::Table(format!("catalog.{relation}").into()),
            "SELECT",
            true,
        ));
    }
    // A gate JUDGES the document it reads and mutates nothing in the catalog
    // (wamn-0h0g.8.5.5). The grant batch revokes every table privilege before
    // granting, so these three are absent — asserted negatively here so a
    // widened ACL fails the surface at startup instead of at the first write it
    // makes possible.
    for privilege in ["INSERT", "UPDATE", "DELETE"] {
        acl.push(AclExpectation::new(
            AclTarget::Table("catalog.wirings".into()),
            privilege,
            false,
        ));
    }
    let expected = ExpectedCredentialIdentity::new(
        // Both users are the generation role: nothing issues `SET ROLE` between
        // connect and this probe, so a differing `current_user` means the session
        // is not the principal the URL named.
        connection.role(),
        connection.role(),
        connection.database(),
        tenant_id,
        vec![MembershipExpectation::new(
            MANAGEMENT_ADMITTER_ROLE,
            MembershipMode::Member,
            true,
        )],
        acl,
    );
    credential_exactness_probe(source, expected)
}

/// One gate command's inputs, already reconciled with the fixed scope.
#[derive(Clone, Copy, Debug)]
pub struct GateRequest<'a> {
    pub environment: &'a str,
    /// The wiring hash. The owner ruled `validated_draft_id` IS the wiring hash:
    /// the draft concept died with the pivot, the wiring document is the
    /// validated artifact, and its hash is the identity.
    pub validated_draft_id: &'a str,
}

/// The one durable fact an ACCEPTED gate produces (wamn-0h0g.8.5.6).
///
/// It is keyed by `wiring_hash` and nothing else: a gate is effect-free, so the
/// verdict is reproducible from the document and mints no identity of its own.
/// `wiring_hash` is therefore both the report's key and the report id the
/// receipt hands back.
///
/// `summary` names the cases the judged document DECLARES. It records no
/// per-case verdict, because nothing was executed — the gate judged the
/// document, and a summary claiming case results would be a lie about work that
/// did not happen.
#[derive(Clone, Debug, PartialEq)]
pub struct GateReport {
    pub wiring_hash: String,
    pub passed: bool,
    pub summary: Value,
}

/// What one gate command judged.
#[derive(Clone, Debug, PartialEq)]
pub enum GateJudgment {
    /// The candidate is gateable. The report this produced is the caller's to
    /// persist: `run_gate` reads the PROJECT database and the report lives in
    /// the CONTROL one, so the verb that holds both connections writes it.
    Accepted(GateReport),
    Refused(GateRefusal),
}

/// Judge one candidate document against the postures that can refuse it.
///
/// A gate is a JUDGMENT ABOUT A DOCUMENT, not an execution of it (wamn-0h0g.8.5.5,
/// ratified spec §5.1), so this reads and refuses; it writes nothing anywhere.
/// The durable report row keyed by `wiring_hash` is `wamn-0h0g.8.5.6`'s to
/// construct.
///
/// The order of the four legs is load-bearing and is the order they landed in:
/// a candidate that does not resolve cannot be judged, a malformed `cases` array
/// is refused before any posture is read, and **the effect-free clause fires
/// before anything else can act on the candidate**.
pub async fn run_gate(
    admission: &AdmissionSurface,
    request: &GateRequest<'_>,
) -> anyhow::Result<GateJudgment> {
    let Some(candidate) = admission
        .candidate_by_hash(request.validated_draft_id)
        .await?
    else {
        return Ok(GateJudgment::Refused(GateRefusal::ValidatedDraftNotFound {
            validated_draft_id: request.validated_draft_id.to_owned(),
        }));
    };
    if let Err(error) = validate_cases(&candidate.cases) {
        return Ok(GateJudgment::Refused(GateRefusal::InvalidTestSet {
            detail: error.to_string(),
        }));
    }

    // THE CONSTITUTIONAL CLAUSE (wamn-0h0g.8.5.5): gate cases are EFFECT-FREE BY
    // CONTRACT. Effects belong to admitted runs under run identity, and a report
    // keyed by content hash must be reproducible from the document alone or that
    // identity is a lie. This refuses BEFORE the candidate is accepted and before
    // any other posture is read, so nothing is performed and then regretted.
    // Assume the clause instead of checking it and the first effectful case
    // silently double-fires. This is the clause's ONE firing point in the tree:
    // it moved here with the gate verb when the composition machinery that used
    // to hold it was deleted, and it did not move out of the way.
    let effectful = admission.effectful_components(&candidate).await?;
    if !effectful.is_empty() {
        return Ok(GateJudgment::Refused(
            GateRefusal::EffectfulComponentReached {
                components: effectful,
            },
        ));
    }

    // A candidate whose store aliases this environment cannot resolve reaches no
    // binding world, so it cannot be judged against one. This used to be read
    // out of the admission statement's `binding-world-unavailable`; with the
    // admission leg deleted the diagnostic that always named the same aliases is
    // the judgment itself.
    let unresolved = admission
        .unresolved_store_aliases(&candidate, request.environment)
        .await?;
    if !unresolved.is_empty() {
        return Ok(GateJudgment::Refused(GateRefusal::DraftConnectionsDenied {
            connection_names: unresolved,
        }));
    }

    // The report identity is DERIVED, never minted: it IS the candidate's
    // content hash. Reached only here, after every refusing posture — the
    // effect-free clause above included — has already declined to fire.
    Ok(GateJudgment::Accepted(GateReport {
        summary: serde_json::json!({
            "cases": candidate
                .cases
                .iter()
                .map(|case| case.case_id.as_str())
                .collect::<Vec<_>>(),
        }),
        wiring_hash: candidate.wiring_hash,
        passed: true,
    }))
}

/// Read a candidate's `cases` array out of its stored graph.
///
/// The array is the flow document's own, so it is decoded with the contract type
/// and held to the contract's bounds rather than to a second declaration of
/// them.
fn candidate_cases(graph: &Value) -> anyhow::Result<Vec<TestSetCase>> {
    let Some(cases) = graph.get("cases") else {
        return Ok(Vec::new());
    };
    serde_json::from_value(cases.clone()).context("decode the candidate's stored cases array")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_candidate_carrying_no_cases_reads_as_an_empty_selection() {
        assert_eq!(candidate_cases(&json!({"nodes": {}})).unwrap(), Vec::new());
        assert_eq!(candidate_cases(&json!({"cases": []})).unwrap(), Vec::new());
    }

    #[test]
    fn the_stored_cases_array_decodes_as_the_contract_type() {
        let cases = candidate_cases(&json!({
            "cases": [{
                "case-id": "roundtrip",
                "input": {"a": 1},
                "expect": {"outcome": "responded", "status": 201},
            }],
        }))
        .expect("a well-formed cases array decodes");
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case_id, "roundtrip");
        assert_eq!(cases[0].expect.status, Some(201));
        // The contract type denies unknown fields, so a foreign array is refused
        // here rather than silently narrowed.
        assert!(
            candidate_cases(&json!({"cases": [{"case-id": "x", "input": {}, "why": 1}]})).is_err()
        );
    }


    /// The effect-posture read is EXACTLY the `wamn-0h0g.21.9` fact, resolved
    /// over exactly the components a run would reach.
    ///
    /// This is a static statement built in Rust, so its text is the contract and
    /// is pinned whole. What each clause buys:
    ///
    /// - it reads `catalog.component_library.effects` and nothing else, so it
    ///   is a third READER of the admitted posture rather than a second
    ///   derivation of it;
    /// - `jsonb_array_length(...) > 0` is the non-empty test, so the empty
    ///   projection — the validator's POSITIVE "leaves the host nowhere" fact —
    ///   is the only thing that passes;
    /// - the join keys are the same four the store-alias diagnostic uses, so a
    ///   gate cannot resolve a different component set than a run does.
    #[test]
    fn the_effect_posture_read_is_the_admitted_projection_and_nothing_else() {
        let sql = SELECT_EFFECTFUL_COMPONENTS_SQL;
        assert_eq!(
            sql,
            "WITH node AS ( \
                SELECT entry.value ->> 'component' AS component, \
                       entry.value ->> 'interface-version' AS interface_version, \
                       entry.value ->> 'operation' AS operation \
                  FROM jsonb_each($4::jsonb) AS entry \
            ) \
            SELECT DISTINCT library.component \
              FROM node JOIN catalog.component_library AS library \
                ON library.tenant_id = $1 AND library.catalog_id = $2 \
               AND library.catalog_version = $3 \
               AND library.component = node.component \
               AND library.interface_version = node.interface_version \
               AND library.operation = node.operation \
             WHERE jsonb_array_length(library.effects) > 0 \
             ORDER BY 1"
        );
        // The refusal is a judgment, never a mutation: a gate that wrote
        // anything on this path would not be a judgment about a document.
        for mutation in ["INSERT", "UPDATE", "DELETE", "TRUNCATE"] {
            assert!(
                !sql.contains(mutation),
                "the posture read performs {mutation}"
            );
        }
        // It resolves components over the same four join keys the binding-world
        // diagnostic does, so the two agree on what the document reaches.
        for shared in [
            "library.component = node.component",
            "library.interface_version = node.interface_version",
            "library.operation = node.operation",
            "library.catalog_version = $3",
        ] {
            assert!(
                SELECT_UNRESOLVED_STORE_ALIASES_SQL.contains(shared),
                "the two candidate resolutions disagree on {shared}"
            );
        }
    }

    /// A candidate whose graph carries no `nodes` object reaches no component,
    /// and the value handed to `jsonb_each` is an object rather than a null.
    #[test]
    fn a_candidate_with_no_nodes_object_is_read_as_reaching_nothing() {
        let candidate = |graph: Value| CandidateWiring {
            catalog_id: "catalog-a".to_owned(),
            catalog_version: 1,
            wiring_id: "wiring-a".to_owned(),
            wiring_version: 1,
            wiring_hash: "sha256:".to_owned() + &"0".repeat(64),
            cases: Vec::new(),
            nodes: graph.get("nodes").cloned().unwrap_or(Value::Null),
        };
        assert_eq!(
            candidate(json!({"cases": []})).nodes_object(),
            json!({}),
            "an absent nodes object must not reach jsonb_each as null"
        );
        assert_eq!(candidate(json!({"nodes": []})).nodes_object(), json!({}));
        let nodes = json!({"a": {"component": "c", "interface-version": "1", "operation": "op"}});
        assert_eq!(candidate(json!({"nodes": nodes})).nodes_object(), nodes);
    }

    /// The expected identity is DERIVED from the parsed connection, never
    /// hand-copied beside it.
    ///
    /// `credential_exactness_probe` refuses a user, database or tenant that the
    /// source and the expectation disagree on, before a socket is used. So this
    /// building at all is the assertion: a role or database name restated by
    /// hand would refuse here, without a server.
    #[test]
    fn the_credential_probe_binds_the_parsed_generation_identity() {
        const ORG: &str = "acme";
        const PROJECT: &str = "receiving";
        const ENVIRONMENT: &str = "dev";
        const DATABASE: &str = "wamn-db-acme--receiving--dev--k3m9x2p7";

        let role = wamn_control_provision::management_admitter_generation_role(
            ORG,
            PROJECT,
            ENVIRONMENT,
            DATABASE,
            wamn_control_provision::CredentialGeneration::A,
        );
        let url = format!("postgres://{role}:secret@project.invalid:5432/{DATABASE}");
        let connection = parse_management_admission_url(&url, ORG, PROJECT, ENVIRONMENT)
            .expect("an in-scope admission URL");
        admission_credential_probe(&url, &connection, "tenant-a")
            .expect("the parsed identity is the expected identity");
    }
}
