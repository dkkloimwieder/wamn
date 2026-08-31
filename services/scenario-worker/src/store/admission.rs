//! Gate and Publish access to the scoped PROJECT database.
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
//! The surface is deliberately narrow: gate reads the two postures that can
//! refuse a document, while Publish reads current component facts and appends
//! exactly seven `catalog.wirings` columns after the control-store green-report
//! guard. That is the whole of what the admitter credential is granted
//! (`MANAGEMENT_ADMITTER_*` in `crates/control/provision/src/sql.rs`). A column
//! absent from those lists is DENIED, not merely unmentioned, so a wider query
//! fails closed at runtime rather than compile-time.
//!
//! # How this connection reaches a governed row at all (`wamn-0h0g.22.17`)
//!
//! Not by a claim. `wamn_management_admitter` is a project-environment-scoped
//! family — `WorkloadRoleScope::ProjectEnvironment` has no tenant field, so its
//! login name never encoded one — and the tenant floor derives its key from
//! `current_user`. The floor is narrowed `TO wamn_app`, and PostgreSQL
//! default-denies when RLS is enabled and no policy matches the connected role,
//! so before this bead the admitter read every one of these relations at
//! `ERROR: permission denied for function current_tenant_key`. It now reaches
//! them through the one permissive `TO wamn_platform` arm each governed relation
//! carries, which admits every tenant in the database; the `tenant_id = $1`
//! predicates on reads and the surface-owned `tenant_id` value on Publish are
//! what narrow every operation back down.

use anyhow::{Context as _, bail};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};

use wamn_authoring_model::GateRefusal;
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentParameter, AdmittedComponentPort,
    ComponentPackageScope, WiringDocument, validate_wiring_compatibility,
};
use wamn_control_provision::{
    MANAGEMENT_ADMITTER_ROLE, ManagementAdmissionConnection, parse_management_admission_url, sql,
};
use wamn_execution_contract::{TestSetCase, validate_cases};
use wamn_runtime::plugins::wamn_postgres::{
    AclExpectation, AclTarget, AmbientCredentialState, CredentialExactnessProbe,
    CredentialProbeError, ExpectedCredentialIdentity, MembershipExpectation, MembershipMode,
    credential_exactness_probe, explicit_credential_source,
};

/// Pin the session's `search_path` before any admission read.
///
/// # The `app.tenant` injection that used to be here is DELETED, not moved
///
/// It claimed that every relation this connection reads keys its row policy on
/// `NULLIF(current_setting('app.tenant', true), '')`. That stopped being true at
/// `wamn-0h0g.22.6.3`, which swept all 43 guest-reachable relations onto
/// `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`
/// — a derivation from `current_user`, precisely so that a session CANNOT set
/// its own tenant. The `set_config` call had therefore been buying nothing for
/// several waves: it wrote a GUC no surviving policy reads.
///
/// Tenant scoping on this connection is not RLS-shaped and never was. The
/// admitter is `wamn_management_admitter`, a project-environment-scoped family
/// whose login name carries no tenant at all, so it reaches these relations
/// through the permissive `TO wamn_platform` arm (`wamn-0h0g.22.17`) and sees
/// every tenant the database holds. What narrows it is the EXPLICIT
/// `tenant_id = $1` predicate every statement below carries — which is where the
/// scoping was already being done, and now the only place it is claimed.
///
/// The `search_path` pin STAYS and is load-bearing: it resolves every unqualified
/// builtin these statements and the server-side triggers reach through
/// `pg_catalog` alone, closing the search-path hijack that
/// `wamn_authority`'s own function bodies close the same way.
const ADMISSION_SCOPE_SQL: &str =
    "SELECT pg_catalog.set_config('search_path', 'pg_catalog', false)";

/// Read the same complete admitted component facts the compatibility validator
/// receives on the CLI authoring path.
const SELECT_COMPONENT_FACTS_SQL: &str = "\
SELECT component, interface_version, operation, registered_operation, component_digest, \
       imports::text, imports_fingerprint, input_ports::text, \
       output_ports::text, parameters::text, effects::text \
  FROM catalog.component_library \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY component COLLATE \"C\", interface_version COLLATE \"C\"";

/// Append exactly the seven columns management `Publish` is granted.
const INSERT_WIRING_SQL: &str = "\
INSERT INTO catalog.wirings (\
       tenant_id, package_id, package_version, wiring_id, version, \
       graph_json, wiring_hash\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7) \
ON CONFLICT DO NOTHING";

/// Distinguish an exact retry from a conflicting immutable definition.
const EXACT_WIRING_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM catalog.wirings \
     WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
       AND wiring_id = $4 AND version = $5 AND graph_json = $6::text::jsonb \
       AND wiring_hash = $7\
    )";

// THE CANDIDATE LOOKUP IS DELETED (wamn-0h0g.8.28), not moved.
//
// A query of `catalog.wirings` by the submitted document's hash stood here and
// resolved the gate's candidate from the STORED ROW. It was leftover coupling
// from the retired reservation protocol, and execution refuted it: authorship
// refuses to write that row without a green report for its own hash, and this
// was the only producer of the report. Nothing could be gated a first time, per
// DOCUMENT — so no bootstrap step would have sufficed either.
//
// The gate now reads `catalog.wirings` NOT AT ALL. Its candidate is the document
// the command carries, which is what the ratified stateless-gate model meant by
// a report REPRODUCIBLE FROM THE DOCUMENT.

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
/// own applied package version, exactly as the store-alias diagnostic below
/// resolves it, so a gate and a run agree on which components a document reaches.
/// A node naming no library row contributes nothing here because `run_gate`'s
/// compatibility validation refuses it before this posture is read.
///
/// Params: `$1` tenant, `$2` package id, `$3` package version, `$4` nodes.
const SELECT_EFFECTFUL_COMPONENTS_SQL: &str = "WITH node AS ( \
        SELECT entry.value ->> 'component' AS component, \
               entry.value ->> 'interface-version' AS interface_version, \
               entry.value ->> 'operation' AS operation \
          FROM jsonb_each($4::jsonb) AS entry \
    ) \
    SELECT DISTINCT library.component \
      FROM node JOIN catalog.component_library AS library \
        ON library.tenant_id = $1 AND library.package_id = $2 \
       AND library.package_version = $3 \
       AND library.component = node.component \
       AND library.interface_version = node.interface_version \
       AND library.operation = node.operation \
     WHERE jsonb_array_length(library.effects) > 0 \
     ORDER BY 1";

/// Name the store aliases the candidate requires and this environment cannot
/// resolve.
///
/// This mirrors the `requirements` / `resolved_requirements` legs of run
/// admission, including instance lifecycle and active generation, so a refusal
/// names exactly the aliases whose absence would produce
/// [`AdmissionResult::BindingWorldUnavailable`]. It is a diagnostic read only.
const SELECT_UNRESOLVED_STORE_ALIASES_SQL: &str = "WITH node AS ( \
        SELECT entry.value ->> 'component' AS component, \
               entry.value ->> 'interface-version' AS interface_version, \
               entry.value ->> 'operation' AS operation \
          FROM jsonb_each($4::jsonb) AS entry \
    ), component AS ( \
        SELECT DISTINCT library.component_digest \
          FROM node JOIN catalog.component_library AS library \
            ON library.tenant_id = $1 AND library.package_id = $2 \
           AND library.package_version = $3 \
           AND library.component = node.component \
           AND library.interface_version = node.interface_version \
           AND library.operation = node.operation \
    ), release_scope AS ( \
        SELECT head.effective_release_id \
          FROM catalog.effective_release_heads AS head \
          JOIN catalog.effective_release_packages AS member \
            ON member.tenant_id = head.tenant_id \
           AND member.effective_release_id = head.effective_release_id \
           AND member.package_id = $2 AND member.package_version = $3 \
         WHERE head.tenant_id = $1 AND head.environment = $5 \
    ), requirement AS ( \
        SELECT required.component_digest, required.store_alias \
          FROM component JOIN catalog.connection_requirements AS required \
            ON required.tenant_id = $1 \
           AND required.component_digest = component.component_digest \
    ) \
    SELECT DISTINCT requirement.store_alias \
      FROM requirement \
      LEFT JOIN release_scope ON true \
      LEFT JOIN catalog.connection_bindings AS binding \
        ON binding.tenant_id = $1 \
       AND binding.effective_release_id = release_scope.effective_release_id \
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
    pub package_id: String,
    /// Exact package version whose admitted facts judge this definition.
    pub package_version: String,
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

/// Project-side result of parsing and compatibility-checking one publication.
#[derive(Debug)]
pub(crate) enum PreparePublishResult {
    Ready(PreparedWiringPublication),
    InvalidDocument {
        detail: String,
    },
    /// The parsed document does not match the current admitted component facts.
    ExecutableDrift,
}

/// Publication facts derived by the server and opaque to the management route.
///
/// Keeping every stored value private makes the route's green-report lookup a
/// guard over the SAME hash the append uses; no caller-supplied hash can enter
/// the write after that check.
#[derive(Debug)]
pub(crate) struct PreparedWiringPublication {
    tenant_id: Box<str>,
    package_id: Box<str>,
    package_version: Box<str>,
    document: WiringDocument,
    stored_version: i32,
    graph_json: String,
    wiring_hash: Box<str>,
}

impl PreparedWiringPublication {
    pub(crate) fn wiring_id(&self) -> &str {
        &self.document.wiring_id
    }

    pub(crate) fn version(&self) -> u32 {
        self.document.version
    }

    pub(crate) fn wiring_hash(&self) -> &str {
        &self.wiring_hash
    }
}

/// Result of the immutable project-side append after the caller's green guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppendPublishResult {
    Published,
    ExecutableDrift,
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
            .query_one(ADMISSION_SCOPE_SQL, &[])
            .await
            .context("pin the admission session search path")?;
        Ok(())
    }

    /// Parse, validate and derive the immutable facts for management `Publish`.
    ///
    /// This phase deliberately does not write. Its opaque result exposes only
    /// the derived identity the management verb needs to read the CONTROL
    /// store's gate report. The caller may pass it to [`Self::append_publish`]
    /// only after that exact report exists and is green.
    pub(crate) async fn prepare_publish(
        &self,
        package_id: &str,
        package_version: &str,
        submitted_document: &Value,
    ) -> anyhow::Result<PreparePublishResult> {
        let document = match WiringDocument::parse(submitted_document) {
            Ok(document) => document,
            Err(error) => {
                return Ok(PreparePublishResult::InvalidDocument {
                    detail: error.to_string(),
                });
            }
        };
        let stored_version = match i32::try_from(document.version) {
            Ok(version) => version,
            Err(error) => {
                return Ok(PreparePublishResult::InvalidDocument {
                    detail: format!(
                        "wiring {:?} version {} exceeds the storage width: {error}",
                        document.wiring_id, document.version
                    ),
                });
            }
        };
        let scope = ComponentPackageScope::new(
            self.tenant_id.to_string(),
            package_id.to_owned(),
            package_version.to_owned(),
        )
        .context("publication package coordinate is invalid")?;
        let components = self.component_facts(&scope).await?;
        let wiring_hash = document.wiring_hash().as_str().to_owned();
        if validate_wiring_compatibility(&document, &scope, &components).is_err() {
            return Ok(PreparePublishResult::ExecutableDrift);
        }
        let graph_json = serde_json::to_string(&document)
            .context("serialize the validated wiring document for storage")?;
        Ok(PreparePublishResult::Ready(PreparedWiringPublication {
            tenant_id: self.tenant_id.clone(),
            package_id: package_id.into(),
            package_version: package_version.into(),
            document,
            stored_version,
            graph_json,
            wiring_hash: wiring_hash.into(),
        }))
    }

    /// Append one validated publication after the management verb's green-report
    /// guard, converging an exact retry and refusing conflicting immutable facts.
    pub(crate) async fn append_publish(
        &self,
        publication: &PreparedWiringPublication,
    ) -> anyhow::Result<AppendPublishResult> {
        if publication.tenant_id.as_ref() != self.tenant_id.as_ref() {
            bail!("prepared publication belongs to another admission tenant");
        }
        let tenant_id = self.tenant_id.as_ref();
        let package_id = publication.package_id.as_ref();
        let package_version = publication.package_version.as_ref();
        let wiring_id = publication.wiring_id();
        let wiring_hash = publication.wiring_hash();
        let parameters: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
            &tenant_id,
            &package_id,
            &package_version,
            &wiring_id,
            &publication.stored_version,
            &publication.graph_json,
            &wiring_hash,
        ];
        self.client
            .execute(INSERT_WIRING_SQL, &parameters)
            .await
            .context("append the gated wiring definition")?;
        let exact: bool = self
            .client
            .query_one(EXACT_WIRING_SQL, &parameters)
            .await
            .context("verify the stored wiring definition")?
            .get(0);
        Ok(if exact {
            AppendPublishResult::Published
        } else {
            AppendPublishResult::ExecutableDrift
        })
    }

    async fn component_facts(
        &self,
        scope: &ComponentPackageScope,
    ) -> anyhow::Result<Vec<AdmittedComponent>> {
        let rows = self
            .client
            .query(
                SELECT_COMPONENT_FACTS_SQL,
                &[&scope.tenant_id, &scope.package_id, &scope.package_version],
            )
            .await
            .context("read the publication scope's admitted component facts")?;
        rows.into_iter()
            .map(|row| {
                let component: String = row.get(0);
                let decoded = AdmittedComponent {
                    scope: scope.clone(),
                    component: component.clone(),
                    interface_version: row.get(1),
                    operation: row.get(2),
                    registered_operation: row.get(3),
                    component_digest: row.get(4),
                    imports: decode_component_json(row.get(5), &component, "imports")?,
                    imports_fingerprint: row.get(6),
                    input_ports: decode_component_json::<Vec<AdmittedComponentPort>>(
                        row.get(7),
                        &component,
                        "input-ports",
                    )?,
                    output_ports: decode_component_json::<Vec<AdmittedComponentPort>>(
                        row.get(8),
                        &component,
                        "output-ports",
                    )?,
                    parameters: decode_component_json::<Vec<AdmittedComponentParameter>>(
                        row.get(9),
                        &component,
                        "parameters",
                    )?,
                    effects: decode_component_json::<Vec<AdmittedComponentEffect>>(
                        row.get(10),
                        &component,
                        "effects",
                    )?,
                };
                wamn_catalog::verify_stored_effect_projection(&decoded).with_context(|| {
                    format!(
                        "component {component:?} stores an effect projection its audited imports \
                         do not derive"
                    )
                })?;
                Ok(decoded)
            })
            .collect()
    }

    /// Name the effectful components this candidate's gate cases reach.
    ///
    /// Empty means this posture permits the candidate: every component it reaches
    /// carries the empty effects projection, which is the POSITIVE fact the
    /// validator derived rather than the absence of one.
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
                    &candidate.package_id,
                    &candidate.package_version,
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
                    &candidate.package_id,
                    &candidate.package_version,
                    &candidate.nodes_object(),
                    &environment,
                ],
            )
            .await
            .context("name the candidate's unresolvable store aliases")?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }
}

fn decode_component_json<T: DeserializeOwned>(
    stored: String,
    component: &str,
    field: &'static str,
) -> anyhow::Result<T> {
    serde_json::from_str(&stored)
        .with_context(|| format!("component {component:?} stores unreadable {field}"))
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
    let acl = admission_acl_expectations();
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

fn admission_acl_expectations() -> Vec<AclExpectation> {
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
    // Publish appends exactly the seven values its project-side statement names.
    // The table-wide negative stays alongside them: a column-exact grant must
    // not silently widen into authority over future columns.
    for column in sql::MANAGEMENT_ADMITTER_WIRING_INSERT_COLUMNS {
        acl.push(AclExpectation::new(
            AclTarget::Column {
                relation: "catalog.wirings".into(),
                column: column.into(),
            },
            "INSERT",
            true,
        ));
    }
    acl.push(AclExpectation::new(
        AclTarget::Column {
            relation: "catalog.wirings".into(),
            column: "created_at".into(),
        },
        "INSERT",
        false,
    ));
    for privilege in ["INSERT", "UPDATE", "DELETE"] {
        acl.push(AclExpectation::new(
            AclTarget::Table("catalog.wirings".into()),
            privilege,
            false,
        ));
    }
    acl
}

/// One gate command's inputs, already reconciled with the fixed scope.
///
/// The DOCUMENT is the candidate (wamn-0h0g.8.28). Nothing here names a stored
/// row, and the identity the report is keyed by is derived from these bytes
/// rather than accepted from the caller.
#[derive(Clone, Copy, Debug)]
pub struct GateRequest<'a> {
    pub environment: &'a str,
    /// Package identity whose admitted component facts judge this document.
    pub package_id: &'a str,
    /// Exact package version those facts are read at.
    pub package_version: &'a str,
    /// The submitted wiring document, already validated by
    /// [`wamn_catalog::WiringDocument::parse`].
    pub document: &'a wamn_catalog::WiringDocument,
}

/// The one durable fact an ACCEPTED gate produces (wamn-0h0g.8.5.6).
///
/// It is keyed by `wiring_hash` and nothing else: a gate is effect-free, so the
/// verdict is reproducible from the document and mints no identity of its own.
/// `wiring_hash` is therefore both the report's key and the report id the
/// receipt hands back.
///
/// `summary` counts the cases the judged document declares. It records no
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
/// is refused before any posture is read, and **a nonempty case set's effect-free
/// clause fires before anything else can act on the candidate**.
pub async fn run_gate(
    admission: &AdmissionSurface,
    request: &GateRequest<'_>,
) -> anyhow::Result<GateJudgment> {
    // The candidate is the DOCUMENT (wamn-0h0g.8.28). It arrives already through
    // `WiringDocument::parse` — the one validating reader for these bytes — and
    // the identity the report is keyed by is DERIVED from what that accepted,
    // never taken from the caller (wamn-0h0g.7.8).
    let document = request.document;
    let candidate = CandidateWiring {
        package_id: request.package_id.to_owned(),
        package_version: request.package_version.to_owned(),
        wiring_id: document.wiring_id.clone(),
        wiring_version: i32::try_from(document.version)
            .context("wiring version exceeds the PostgreSQL integer carrier")?,
        wiring_hash: document.wiring_hash().as_str().to_owned(),
        cases: document.cases.clone(),
        nodes: serde_json::to_value(&document.nodes)
            .context("re-serialize the judged document's nodes")?,
    };
    if !candidate.cases.is_empty()
        && let Err(error) = validate_cases(&candidate.cases)
    {
        return Ok(GateJudgment::Refused(GateRefusal::InvalidTestSet {
            detail: error.to_string(),
        }));
    }
    let component_scope = match ComponentPackageScope::new(
        admission.tenant_id.to_string(),
        request.package_id,
        request.package_version,
    ) {
        Ok(scope) => scope,
        Err(error) => {
            return Ok(GateJudgment::Refused(GateRefusal::InvalidDocument {
                detail: error.to_string(),
            }));
        }
    };
    let components = admission.component_facts(&component_scope).await?;
    if let Err(error) = validate_wiring_compatibility(document, &component_scope, &components) {
        return Ok(GateJudgment::Refused(GateRefusal::InvalidDocument {
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
    // With no cases there is no execution posture to read: treating the nodes'
    // effects alone as a refusal would turn this case contract into a blanket
    // ban on effectful production wiring. The binding-world posture below still
    // judges the document in either arm.
    if !candidate.cases.is_empty() {
        let effectful = admission.effectful_components(&candidate).await?;
        if !effectful.is_empty() {
            return Ok(GateJudgment::Refused(
                GateRefusal::EffectfulComponentReached {
                    components: effectful,
                },
            ));
        }
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
            "cases": candidate.cases.len(),
        }),
        wiring_hash: candidate.wiring_hash,
        passed: true,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The cases array is the DOCUMENT's own, decoded by the contract type.
    ///
    /// This replaces two tests of a `candidate_cases` helper that read the array
    /// out of a stored `graph_json` blob. wamn-0h0g.8.28 deleted that helper with
    /// the stored-row lookup it served: the gate now receives an already-parsed
    /// `WiringDocument`, so the same bound is held by the same contract type one
    /// layer earlier, and `deny_unknown_fields` refuses a foreign array there.
    #[test]
    fn the_documents_cases_array_is_the_contract_type() {
        let document = wamn_catalog::WiringDocument::parse(&json!({
            "format-version": "0.1",
            "wiring-id": "orders-create",
            "version": 1,
            "entry": "node",
            "nodes": {"node": {
                "component": "entity",
                "interface-version": "0.1",
                "operation": "create",
            }},
            "cases": [{
                "case-id": "roundtrip",
                "input": {"a": 1},
                "expect": {"outcome": "responded", "status": 201},
            }],
        }))
        .expect("a well-formed document parses");
        assert_eq!(document.cases.len(), 1);
        assert_eq!(document.cases[0].case_id, "roundtrip");
        assert_eq!(document.cases[0].expect.status, Some(201));

        // A document carrying no cases reaches an empty selection, not an error.
        let bare = wamn_catalog::WiringDocument::parse(&json!({
            "format-version": "0.1",
            "wiring-id": "orders-create",
            "version": 1,
            "entry": "node",
            "nodes": {"node": {
                "component": "entity",
                "interface-version": "0.1",
                "operation": "create",
            }},
        }))
        .expect("a document with no cases parses");
        assert!(bare.cases.is_empty());

        // A foreign field in a case is REFUSED, not silently narrowed.
        assert!(
            wamn_catalog::WiringDocument::parse(&json!({
                "format-version": "0.1",
                "wiring-id": "orders-create",
                "version": 1,
                "entry": "node",
                "nodes": {"node": {
                    "component": "entity",
                    "interface-version": "0.1",
                    "operation": "create",
                }},
                "cases": [{"case-id": "x", "input": {}, "why": 1}],
            }))
            .is_err()
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
                ON library.tenant_id = $1 AND library.package_id = $2 \
               AND library.package_version = $3 \
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
            "library.package_version = $3",
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
            package_id: "package_a".to_owned(),
            package_version: "1.0.0".to_owned(),
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

    /// The builder and the live credential probe agree on the exact seven-column
    /// Publish append, while the probe still refuses table-wide INSERT and the
    /// defaulted storage timestamp (`wamn-0h0g.7.7`).
    #[test]
    fn the_publish_surface_is_column_exact_at_the_runtime_boundary() {
        const INSERT_COLUMNS: [&str; 7] = [
            "tenant_id",
            "package_id",
            "package_version",
            "wiring_id",
            "version",
            "graph_json",
            "wiring_hash",
        ];
        assert_eq!(
            sql::MANAGEMENT_ADMITTER_WIRING_INSERT_COLUMNS,
            INSERT_COLUMNS
        );
        let acl = admission_acl_expectations();
        for column in INSERT_COLUMNS {
            assert!(acl.contains(&AclExpectation::new(
                AclTarget::Column {
                    relation: "catalog.wirings".into(),
                    column: column.into(),
                },
                "INSERT",
                true,
            )));
        }
        for forbidden in [
            AclExpectation::new(
                AclTarget::Column {
                    relation: "catalog.wirings".into(),
                    column: "created_at".into(),
                },
                "INSERT",
                false,
            ),
            AclExpectation::new(AclTarget::Table("catalog.wirings".into()), "INSERT", false),
        ] {
            assert!(acl.contains(&forbidden));
        }
        assert_eq!(
            acl.len(),
            20,
            "schema + eight reads + seven inserts + omitted column + three table negatives"
        );
    }

    /// These Rust-built statements are the artifact: pin the full write and the
    /// exact retry check so the green-report hash cannot be replaced on append.
    #[test]
    fn publication_append_uses_only_the_server_derived_exact_identity() {
        assert_eq!(
            INSERT_WIRING_SQL,
            "INSERT INTO catalog.wirings (tenant_id, package_id, package_version, wiring_id, \
             version, graph_json, wiring_hash) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7) \
             ON CONFLICT DO NOTHING"
        );
        assert_eq!(
            EXACT_WIRING_SQL,
            "SELECT EXISTS (SELECT 1 FROM catalog.wirings \
             WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
             AND wiring_id = $4 AND version = $5 AND graph_json = $6::text::jsonb \
             AND wiring_hash = $7)"
        );
        assert!(!INSERT_WIRING_SQL.contains("DO UPDATE"));
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
