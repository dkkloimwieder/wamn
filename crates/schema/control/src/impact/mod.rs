//! wamn schema-change impact analysis (11.8).
//!
//! The PURE decision behind `wamn-ctl impact-report` and the `migrate-catalog`
//! acknowledge gate: given a compiled [`MigrationPlan`]
//! and the dependency data a control-plane verb reads from the project database,
//! enumerate — per affected entity — WHAT changes and WHAT downstream depends on
//! it, so a schema designer sees the blast radius *before* any DDL applies.
//!
//! This crate is a JOIN over data the platform already stores; it holds no
//! connection, clock, or wasm. The [`analyze`] inputs are plain data ([`ImpactInput`]);
//! the [`ImpactReport`] output is plain data. The five edges it computes:
//!
//! 1. **affected entity + classification** — group the plan's operations by
//!    [`wamn_schema_compiler::Operation::entity`]; an entity is destructive iff any of its ops
//!    is [`wamn_schema_compiler::Safety::Destructive`] (the plan is the authoritative source —
//!    no SQL re-parse).
//! 2. **flows via event registration** — id-keyed and rename-proof: registrations
//!    whose stable `entity_id` equals the affected entity's id
//!    (`catalog.event_registrations`, the `event_registrations_by_entity` index).
//! 3. **flows via node config** — NAME-keyed and NOT rename-proof: an active
//!    flow's `postgres` node names its entity in `config["entity"]`, resolved by
//!    the generated router *by entity name* (`wamn_api`). A rename silently
//!    dangles the ref; surfacing it (by the OLD name) is a genuine report line
//!    ([`nodescan`]).
//! 4. **suites of affected flows** — every `test_suites` row of a flow the entity
//!    change touches (all versions; the suite tuple keeps its version so the
//!    parked executor, wamn-0lfu, can pin).
//! 5. **generated-API resources** — pure over the catalog: the entity's own
//!    `/api/rest/{name}` plus the neighbours' `?expand=` resources that embed it.
//!
//! **Out of scope (parked wamn-0lfu, "execution from stored suites").** The report
//! *enumerates* the `(tenant, flow_id, flow_version, suite_id)` tuples that WOULD
//! run; it never executes them. [`ImpactReport`]'s suite tuples are that executor's
//! input contract: [`suite_selectors`] (and [`suite_selectors_json`]) flatten
//! `entities[].suites[]` into the exact `--impact-report` array it reads, which is
//! the seam the wamn-12g `migrate-catalog` auto-run wiring calls.

pub mod nodescan;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wamn_flow::Flow;
use wamn_schema_compiler::MigrationPlan;
use wamn_schema_model::{Catalog, Entity};

// ---------------------------------------------------------------------------
// Inputs — plain data the driver reads from the project database.
// ---------------------------------------------------------------------------

/// One event registration (a subscribing flow's declaration), keyed on the stable
/// entity id — the rename-proof edge. Rows come from `catalog.event_registrations`
/// across ALL tenants (a shared entity's change hits every tenant's registration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationEdge {
    pub tenant: String,
    pub flow_id: String,
    pub entity_id: String,
    pub registration_id: String,
}

/// One active flow graph (a `<schema>.flows` row where `active`), tagged with its
/// owning tenant — the input to the name-keyed node-config scan.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowGraph {
    pub tenant: String,
    pub flow: Flow,
}

/// One stored test suite (`<schema>.test_suites`), tagged with its owning tenant.
/// A suite pins a concrete `(flow_id, flow_version)`; a suite of a flow the change
/// touches is enumerated regardless of whether that flow version is active.
///
/// This is ALSO the stored-suite executor's `--impact-report` input row: the
/// serialized field names and types (`tenant / flow_id / flow_version: i32 /
/// suite_id`) are the 12g auto-run seam's wire contract, so an unknown field is
/// REFUSED on read. [`suite_selectors`] emits the array; the executor's local
/// deserialize type is pinned to this shape by that gate's
/// `suite_selector_matches_the_suite_edge_shape`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteEdge {
    pub tenant: String,
    pub flow_id: String,
    pub flow_version: i32,
    pub suite_id: String,
}

/// The pure analysis inputs. `current` is the pre-migration applied catalog (the
/// diff/plan source; `None` for a first materialization); `target` is the
/// post-migration catalog. `registrations`, `flows`, and `suites` are read
/// cross-tenant by the superuser driver.
#[derive(Debug, Clone)]
pub struct ImpactInput<'a> {
    pub plan: &'a MigrationPlan,
    pub current: Option<&'a Catalog>,
    pub target: &'a Catalog,
    pub registrations: &'a [RegistrationEdge],
    pub flows: &'a [FlowGraph],
    pub suites: &'a [SuiteEdge],
}

// ---------------------------------------------------------------------------
// Output — the typed report.
// ---------------------------------------------------------------------------

/// How an affected entity changes, relative to the migration. `Changed` covers a
/// rename (an entity kept across both versions with a new `name`), which is
/// exactly the case the name-keyed node-config edge surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityChangeKind {
    Added,
    Removed,
    Changed,
}

impl EntityChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            EntityChangeKind::Added => "added",
            EntityChangeKind::Removed => "removed",
            EntityChangeKind::Changed => "changed",
        }
    }
}

/// A flow that references the entity by a rename-proof event registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowViaRegistration {
    pub tenant: String,
    pub flow_id: String,
    pub registration_id: String,
}

/// A flow whose active graph references the entity by NAME through a postgres
/// node's `config["entity"]` — the not-rename-proof edge. `referenced_name` is the
/// name the node used (the OLD name for a rename, which now dangles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowViaNodeConfig {
    pub tenant: String,
    pub flow_id: String,
    pub flow_version: u32,
    pub node_id: String,
    pub referenced_name: String,
}

/// The impact on a single affected entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityImpact {
    pub entity_id: String,
    /// The entity's display name (its `target` name, or its `current` name for a
    /// removed entity).
    pub entity_name: String,
    pub change: EntityChangeKind,
    /// `true` if any of the entity's plan operations is destructive.
    pub destructive: bool,
    pub flows_via_registration: Vec<FlowViaRegistration>,
    pub flows_via_node_config: Vec<FlowViaNodeConfig>,
    /// The suites of every flow the entity change touches (both edges), all
    /// versions — the tuples the parked executor (wamn-0lfu) WOULD run.
    pub suites: Vec<SuiteEdge>,
    /// The generated-API resources over the catalog: `/api/rest/{name}` plus the
    /// neighbours' `?expand=` resources that embed this entity.
    pub api_resources: Vec<String>,
}

impl EntityImpact {
    /// `true` if some flow or suite depends on this entity (either edge). The
    /// generated-API resources are pure catalog derivations, NOT downstream
    /// dependents — every entity has them — so they do not count here.
    pub fn has_downstream_impact(&self) -> bool {
        !self.flows_via_registration.is_empty()
            || !self.flows_via_node_config.is_empty()
            || !self.suites.is_empty()
    }
}

/// The whole impact report — the affected entities, entity-id ordered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImpactReport {
    pub entities: Vec<EntityImpact>,
}

/// A destructive migration whose downstream impact was not acknowledged (the
/// 11.8 gate). Mirrors [`wamn_schema_compiler::RequiresConfirmation`] /
/// `wamn_schema_control::OrphaningPublish`: a canonical struct error the driver surfaces,
/// naming the destructively-changed entities that carry dependent flows/suites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactNotAcknowledged {
    /// `(entity_name, entity_id)` of each destructive entity with dependents.
    pub entities: Vec<(String, String)>,
}

impl std::fmt::Display for ImpactNotAcknowledged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing this destructive migration: {} affected entit{} carr{} dependent \
             flows/suites — review the impact report and re-run with --acknowledge-impact:",
            self.entities.len(),
            if self.entities.len() == 1 { "y" } else { "ies" },
            if self.entities.len() == 1 { "ies" } else { "y" },
        )?;
        for (name, id) in &self.entities {
            write!(f, "\n  - entity {name:?} (id {id:?})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ImpactNotAcknowledged {}

impl ImpactReport {
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// `true` if any affected entity is destructive.
    pub fn any_destructive(&self) -> bool {
        self.entities.iter().any(|e| e.destructive)
    }

    /// `true` if any affected entity is BOTH destructive AND carries a dependent
    /// flow or suite — the condition the `migrate-catalog` acknowledge gate fires
    /// on. A destructive change with no dependents (or an additive change with
    /// dependents) does not require acknowledgement.
    pub fn requires_acknowledgement(&self) -> bool {
        self.entities
            .iter()
            .any(|e| e.destructive && e.has_downstream_impact())
    }

    /// The typed refusal for an unacknowledged destructive-with-impact plan
    /// ([`requires_acknowledgement`](Self::requires_acknowledgement) must be
    /// `true`), naming the offending entities.
    pub fn acknowledgement_refusal(&self) -> ImpactNotAcknowledged {
        ImpactNotAcknowledged {
            entities: self
                .entities
                .iter()
                .filter(|e| e.destructive && e.has_downstream_impact())
                .map(|e| (e.entity_name.clone(), e.entity_id.clone()))
                .collect(),
        }
    }

    /// A human-readable rendering — mirrors `MigrationReport::render` /
    /// `OrphaningPublish::Display`. The single review surface a schema designer
    /// reads before applying.
    pub fn render(&self) -> String {
        if self.entities.is_empty() {
            return "schema-change impact — no affected entities\n".to_string();
        }
        let mut out = format!(
            "schema-change impact — {} affected entit{}\n",
            self.entities.len(),
            if self.entities.len() == 1 { "y" } else { "ies" },
        );
        for e in &self.entities {
            let tag = if e.destructive {
                "DESTRUCTIVE"
            } else {
                "additive   "
            };
            out.push_str(&format!(
                "  [{tag}] entity {:?} (id {:?}) — {}\n",
                e.entity_name,
                e.entity_id,
                e.change.as_str(),
            ));
            for r in &e.api_resources {
                out.push_str(&format!("      api: {r}\n"));
            }
            for r in &e.flows_via_registration {
                out.push_str(&format!(
                    "      flow via registration: tenant {:?} flow {:?} (registration {:?})\n",
                    r.tenant, r.flow_id, r.registration_id,
                ));
            }
            for n in &e.flows_via_node_config {
                out.push_str(&format!(
                    "      flow via node config:  tenant {:?} flow {:?} v{} node {:?} (config entity {:?})\n",
                    n.tenant, n.flow_id, n.flow_version, n.node_id, n.referenced_name,
                ));
            }
            for s in &e.suites {
                out.push_str(&format!(
                    "      suite: tenant {:?} flow {:?} v{} suite {:?}\n",
                    s.tenant, s.flow_id, s.flow_version, s.suite_id,
                ));
            }
            if !e.has_downstream_impact() {
                out.push_str("      (no dependent flows or suites)\n");
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The auto-run seam — flatten `entities[].suites[]` into the stored-suite
// executor's `--impact-report` selector array (wamn-12g's input).
// ---------------------------------------------------------------------------

/// Why a [`SuiteEdge`] cannot be emitted as an executor selector: an identity
/// field the executor needs to locate the stored suite is missing, or the pinned
/// version is one the wire contract cannot express (the executor casts
/// `flow_version` to the `u32` its flow reference uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteEdgeDefect {
    EmptyTenant,
    EmptyFlowId,
    EmptySuiteId,
    NegativeFlowVersion,
}

impl SuiteEdgeDefect {
    fn as_str(self) -> &'static str {
        match self {
            SuiteEdgeDefect::EmptyTenant => "empty tenant",
            SuiteEdgeDefect::EmptyFlowId => "empty flow_id",
            SuiteEdgeDefect::EmptySuiteId => "empty suite_id",
            SuiteEdgeDefect::NegativeFlowVersion => "negative flow_version",
        }
    }
}

/// A flattening refused because these suite edges would reach the stored-suite
/// executor unable to identify a suite. Mirrors [`crate::OrphaningSuiteCopy`]: a
/// canonical struct error naming every offender, so the caller sees the whole
/// batch instead of one edge at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSuiteEdges {
    /// Each rejected edge with its defect, in [`suite_selectors`] emit order.
    pub rejected: Vec<(SuiteEdge, SuiteEdgeDefect)>,
}

impl std::fmt::Display for InvalidSuiteEdges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to flatten this impact report: {} suite edge(s) would not identify a \
             stored suite:",
            self.rejected.len(),
        )?;
        for (edge, defect) in &self.rejected {
            write!(
                f,
                "\n  - suite {:?} (tenant {:?}) pins {:?} v{}: {}",
                edge.suite_id,
                edge.tenant,
                edge.flow_id,
                edge.flow_version,
                defect.as_str(),
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for InvalidSuiteEdges {}

/// Flatten `report.entities[].suites[]` into the de-duplicated selector array the
/// stored-suite executor's `--impact-report` consumes.
///
/// One suite reachable through several affected entities appears ONCE. The order
/// is `(tenant, flow_id, flow_version, suite_id)` — the same key [`analyze`]
/// sorts each entity's suites by — so repeated runs emit identical bytes.
///
/// # Errors
///
/// [`InvalidSuiteEdges`] if any edge lacks a tenant, flow id, or suite id, or
/// pins a negative version. An incomplete edge REFUSES the whole flattening; it
/// is never silently dropped, because a dropped suite is a suite the auto-run
/// seam would report as passing without ever having executed it.
pub fn suite_selectors(report: &ImpactReport) -> Result<Vec<SuiteEdge>, InvalidSuiteEdges> {
    let mut selectors: Vec<SuiteEdge> = report
        .entities
        .iter()
        .flat_map(|e| e.suites.iter().cloned())
        .collect();
    selectors.sort_by(|a, b| {
        (&a.tenant, &a.flow_id, a.flow_version, &a.suite_id).cmp(&(
            &b.tenant,
            &b.flow_id,
            b.flow_version,
            &b.suite_id,
        ))
    });
    selectors.dedup();

    let rejected: Vec<(SuiteEdge, SuiteEdgeDefect)> = selectors
        .iter()
        .filter_map(|s| suite_edge_defect(s).map(|d| (s.clone(), d)))
        .collect();
    if !rejected.is_empty() {
        return Err(InvalidSuiteEdges { rejected });
    }
    Ok(selectors)
}

/// The exact JSON document the executor reads from `--impact-report <path>`: an
/// array of `{tenant, flow_id, flow_version, suite_id}` objects in
/// [`suite_selectors`] order.
///
/// # Errors
///
/// [`InvalidSuiteEdges`], as [`suite_selectors`].
pub fn suite_selectors_json(report: &ImpactReport) -> Result<String, InvalidSuiteEdges> {
    let selectors = suite_selectors(report)?;
    // Infallible: an array of structs of `String`/`i32` has no non-string map
    // key and no non-finite float, the only ways `to_string_pretty` can fail.
    Ok(serde_json::to_string_pretty(&selectors).expect("suite selector array serializes"))
}

/// The first contract violation in `edge`, or `None` if it can be emitted.
fn suite_edge_defect(edge: &SuiteEdge) -> Option<SuiteEdgeDefect> {
    if edge.tenant.is_empty() {
        Some(SuiteEdgeDefect::EmptyTenant)
    } else if edge.flow_id.is_empty() {
        Some(SuiteEdgeDefect::EmptyFlowId)
    } else if edge.suite_id.is_empty() {
        Some(SuiteEdgeDefect::EmptySuiteId)
    } else if edge.flow_version < 0 {
        Some(SuiteEdgeDefect::NegativeFlowVersion)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// The analysis.
// ---------------------------------------------------------------------------

/// Enumerate the impact of `input.plan` over the dependency data. Deterministic:
/// entities are entity-id ordered and each edge list is sorted, so the render is
/// stable (golden-able).
pub fn analyze(input: &ImpactInput) -> ImpactReport {
    let current_by_id: BTreeMap<&str, &Entity> = input
        .current
        .map(|c| c.entities.iter().map(|e| (e.id.as_str(), e)).collect())
        .unwrap_or_default();
    let target_by_id: BTreeMap<&str, &Entity> = input
        .target
        .entities
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();

    // Edge 1: affected entity ids + destructive classification, from the plan.
    // BTreeMap gives entity-id ordered, deterministic output.
    let mut affected: BTreeMap<&str, bool> = BTreeMap::new();
    for op in &input.plan.operations {
        let d = affected.entry(op.entity.as_str()).or_insert(false);
        *d |= op.safety.is_destructive();
    }

    let mut entities = Vec::with_capacity(affected.len());
    for (id, destructive) in affected {
        let old_name = current_by_id.get(id).map(|e| e.name.as_str());
        let new_name = target_by_id.get(id).map(|e| e.name.as_str());

        let change = match (
            current_by_id.contains_key(id),
            target_by_id.contains_key(id),
        ) {
            (true, true) => EntityChangeKind::Changed,
            (false, true) => EntityChangeKind::Added,
            (true, false) => EntityChangeKind::Removed,
            // The plan attributes an op to an entity in neither catalog — not
            // expected from a valid plan; treat as changed and name it by its id.
            (false, false) => EntityChangeKind::Changed,
        };
        let entity_name = new_name.or(old_name).unwrap_or(id).to_string();

        // The names this entity is known by, for the name-keyed node-config edge.
        // A rename means both old and new appear; a flow using the OLD name now
        // dangles and MUST surface (it references the entity that changed).
        let mut names: BTreeSet<&str> = BTreeSet::new();
        names.extend(old_name);
        names.extend(new_name);

        // Edge 2: flows via event registration (id-keyed, rename-proof).
        let mut flows_via_registration: Vec<FlowViaRegistration> = input
            .registrations
            .iter()
            .filter(|r| r.entity_id == id)
            .map(|r| FlowViaRegistration {
                tenant: r.tenant.clone(),
                flow_id: r.flow_id.clone(),
                registration_id: r.registration_id.clone(),
            })
            .collect();
        flows_via_registration.sort_by(|a, b| {
            (&a.tenant, &a.flow_id, &a.registration_id).cmp(&(
                &b.tenant,
                &b.flow_id,
                &b.registration_id,
            ))
        });

        // Edge 3: flows via node config (NAME-keyed, NOT rename-proof).
        let mut flows_via_node_config: Vec<FlowViaNodeConfig> = Vec::new();
        for fg in input.flows {
            for node in &fg.flow.nodes {
                if let Some(name) = nodescan::node_entity_name(node)
                    && names.contains(name)
                {
                    flows_via_node_config.push(FlowViaNodeConfig {
                        tenant: fg.tenant.clone(),
                        flow_id: fg.flow.flow_id.clone(),
                        flow_version: fg.flow.version,
                        node_id: node.id.clone(),
                        referenced_name: name.to_string(),
                    });
                }
            }
        }
        flows_via_node_config.sort_by(|a, b| {
            (&a.tenant, &a.flow_id, a.flow_version, &a.node_id).cmp(&(
                &b.tenant,
                &b.flow_id,
                b.flow_version,
                &b.node_id,
            ))
        });

        // Edge 4: suites of every flow this entity change touches (both edges),
        // all versions. Affected flows are keyed (tenant, flow_id).
        let affected_flows: BTreeSet<(&str, &str)> = flows_via_registration
            .iter()
            .map(|r| (r.tenant.as_str(), r.flow_id.as_str()))
            .chain(
                flows_via_node_config
                    .iter()
                    .map(|n| (n.tenant.as_str(), n.flow_id.as_str())),
            )
            .collect();
        let mut suites: Vec<SuiteEdge> = input
            .suites
            .iter()
            .filter(|s| affected_flows.contains(&(s.tenant.as_str(), s.flow_id.as_str())))
            .cloned()
            .collect();
        suites.sort_by(|a, b| {
            (&a.tenant, &a.flow_id, a.flow_version, &a.suite_id).cmp(&(
                &b.tenant,
                &b.flow_id,
                b.flow_version,
                &b.suite_id,
            ))
        });

        // Edge 5: generated-API resources (pure over the catalog holding the
        // entity — target, or current for a removed entity).
        let api_resources = api_resources_for(id, input.target, input.current);

        entities.push(EntityImpact {
            entity_id: id.to_string(),
            entity_name,
            change,
            destructive,
            flows_via_registration,
            flows_via_node_config,
            suites,
            api_resources,
        });
    }

    ImpactReport { entities }
}

/// The generated-API resources touching an entity: its own `/api/rest/{name}`
/// plus, for each relation touching it, the neighbour's `?expand=` resource that
/// embeds it (`wamn_api` serves the embed on the OTHER endpoint's resource).
/// Derived from whichever catalog holds the entity (target preferred; current for
/// a removed entity) — pure over the catalog, no `wamn-api` dependency.
fn api_resources_for(id: &str, target: &Catalog, current: Option<&Catalog>) -> Vec<String> {
    let holds = |c: &Catalog| c.entities.iter().any(|e| e.id.as_str() == id);
    let cat: &Catalog = if holds(target) {
        target
    } else if let Some(c) = current.filter(|c| holds(c)) {
        c
    } else {
        // The entity is in neither catalog (unexpected from a valid plan): it has
        // no generated-API resource to name.
        return Vec::new();
    };

    let name_by_id: BTreeMap<&str, &str> = cat
        .entities
        .iter()
        .map(|e| (e.id.as_str(), e.name.as_str()))
        .collect();
    // BTreeSet: sorted + de-duplicated (two relations may name the same neighbour).
    let mut out: BTreeSet<String> = BTreeSet::new();
    if let Some(name) = name_by_id.get(id) {
        out.insert(format!("/api/rest/{name}"));
    }
    for r in &cat.relations {
        let (from, to) = (r.from.as_str(), r.to.as_str());
        if from != id && to != id {
            continue;
        }
        // The neighbour whose `/api/rest/{neighbour}?expand={rel}` embeds THIS
        // entity (a self-referential relation names the entity itself).
        let neighbour = if from == id { to } else { from };
        if let Some(nname) = name_by_id.get(neighbour) {
            out.insert(format!("/api/rest/{nname}?expand={}", r.name));
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_schema_compiler::{Operation, Safety};

    // --- fixture builders ---------------------------------------------------

    /// A plan whose ops attribute `(entity_id, safety)` — the only fields the
    /// analysis reads (per-op entity + classification).
    fn plan(ops: &[(&str, Safety)]) -> MigrationPlan {
        MigrationPlan {
            operations: ops
                .iter()
                .map(|(entity, safety)| Operation {
                    summary: format!("op on {entity}"),
                    sql: String::new(),
                    safety: *safety,
                    entity: (*entity).to_string(),
                    field: None,
                    note: None,
                })
                .collect(),
        }
    }

    /// A catalog of `(id, name)` entities plus `(id, name, from, to)` relations.
    fn cat(entities: &[(&str, &str)], relations: &[(&str, &str, &str, &str)]) -> Catalog {
        let es: Vec<String> = entities
            .iter()
            .map(|(id, name)| {
                format!(
                    r#"{{"id":"{id}","name":"{name}","fields":[{{"id":"f","name":"f","type":{{"kind":"text"}}}}]}}"#
                )
            })
            .collect();
        let rs: Vec<String> = relations
            .iter()
            .map(|(id, name, from, to)| {
                format!(
                    r#"{{"id":"{id}","name":"{name}","cardinality":"one-to-many","from":"{from}","to":"{to}","from-field":"f"}}"#
                )
            })
            .collect();
        let json = format!(
            r#"{{"schema-version":"0.1","catalog-id":"shop","version":1,"entities":[{}],"relations":[{}]}}"#,
            es.join(","),
            rs.join(","),
        );
        Catalog::from_json(&json).expect("catalog fixture parses")
    }

    fn reg(tenant: &str, flow: &str, entity: &str, reg_id: &str) -> RegistrationEdge {
        RegistrationEdge {
            tenant: tenant.into(),
            flow_id: flow.into(),
            entity_id: entity.into(),
            registration_id: reg_id.into(),
        }
    }

    fn suite(tenant: &str, flow: &str, version: i32, suite: &str) -> SuiteEdge {
        SuiteEdge {
            tenant: tenant.into(),
            flow_id: flow.into(),
            flow_version: version,
            suite_id: suite.into(),
        }
    }

    /// A one-node flow whose single `postgres` node references `entity_name` by
    /// its `config["entity"]` (the name-keyed edge).
    fn pg_flow(tenant: &str, flow_id: &str, version: u32, entity_name: &str) -> FlowGraph {
        let json = serde_json::json!({
            "schema-version": "0.1",
            "flow-id": flow_id,
            "version": version,
            "nodes": [ { "id": "read", "type": "postgres", "config": { "entity": entity_name, "op": "get" } } ],
            "edges": [],
        });
        FlowGraph {
            tenant: tenant.into(),
            flow: Flow::from_json(&json.to_string()).expect("flow fixture parses"),
        }
    }

    // --- named mutant killers ----------------------------------------------

    /// MUTANT 1 (invert the entity-match): an UNTOUCHED entity's registration must
    /// never be attributed to a touched entity. Mirrors orphan.rs's `!"r-keep"`.
    #[test]
    fn untouched_entity_flows_are_not_reported() {
        let target = cat(&[("touched", "touched"), ("untouched", "untouched")], &[]);
        let input = ImpactInput {
            plan: &plan(&[("touched", Safety::Destructive)]),
            current: Some(&target),
            target: &target,
            registrations: &[
                reg("t1", "f-touched", "touched", "r-touched"),
                reg("t1", "f-untouched", "untouched", "r-untouched"),
            ],
            flows: &[],
            suites: &[],
        };
        let report = analyze(&input);
        // Only the touched entity is in the report.
        assert_eq!(report.entities.len(), 1);
        let e = &report.entities[0];
        assert_eq!(e.entity_id, "touched");
        // It reports ONLY its own registration — never the untouched entity's.
        assert_eq!(e.flows_via_registration.len(), 1);
        assert_eq!(e.flows_via_registration[0].registration_id, "r-touched");
        let msg = report.render();
        assert!(msg.contains("r-touched"), "{msg}");
        assert!(
            !msg.contains("r-untouched"),
            "untouched reg must not appear: {msg}"
        );
    }

    /// MUTANT 2 (force all ops additive): a destructive change with a dependent
    /// flow MUST require --acknowledge-impact.
    #[test]
    fn destructive_change_with_impact_requires_acknowledge() {
        let target = cat(&[("orders", "orders")], &[]);
        let input = ImpactInput {
            plan: &plan(&[("orders", Safety::Destructive)]),
            current: Some(&target),
            target: &target,
            registrations: &[reg("t1", "notify", "orders", "r1")],
            flows: &[],
            suites: &[],
        };
        let report = analyze(&input);
        assert!(report.any_destructive());
        assert!(
            report.requires_acknowledgement(),
            "a destructive change with a dependent flow needs acknowledgement"
        );
        // The refusal names the entity.
        let refusal = report.acknowledgement_refusal().to_string();
        assert!(
            refusal.contains("orders") && refusal.contains("--acknowledge-impact"),
            "{refusal}"
        );
    }

    /// MUTANT 2 negative: a destructive change with NO dependents does not require
    /// acknowledgement; an additive change with dependents does not either.
    #[test]
    fn acknowledge_is_not_required_without_a_dependent_destructive_change() {
        let target = cat(&[("orders", "orders"), ("audit", "audit")], &[]);
        // orders: destructive but no dependents. audit: additive with a dependent.
        let input = ImpactInput {
            plan: &plan(&[("orders", Safety::Destructive), ("audit", Safety::Additive)]),
            current: Some(&target),
            target: &target,
            registrations: &[reg("t1", "log", "audit", "r-audit")],
            flows: &[],
            suites: &[],
        };
        let report = analyze(&input);
        assert!(report.any_destructive());
        assert!(!report.requires_acknowledgement());
    }

    /// MUTANT 3 (key node-config on entity.id instead of entity.name): the config
    /// edge matches the entity NAME. Fixture with id != name proves it.
    #[test]
    fn node_config_edge_keys_on_entity_name_not_id() {
        // id "sales_orders" but NAME "orders"; the postgres node references "orders".
        let target = cat(&[("sales_orders", "orders")], &[]);
        let input = ImpactInput {
            plan: &plan(&[("sales_orders", Safety::Destructive)]),
            current: Some(&target),
            target: &target,
            registrations: &[],
            flows: &[pg_flow("t1", "sync", 3, "orders")],
            suites: &[],
        };
        let report = analyze(&input);
        let e = &report.entities[0];
        assert_eq!(
            e.flows_via_node_config.len(),
            1,
            "the node referencing the entity NAME is found"
        );
        let n = &e.flows_via_node_config[0];
        assert_eq!(n.flow_id, "sync");
        assert_eq!(n.flow_version, 3);
        assert_eq!(n.referenced_name, "orders");
        // A node referencing the ID (not the name) must NOT match.
        let id_input = ImpactInput {
            flows: &[pg_flow("t1", "wrong", 1, "sales_orders")],
            ..input
        };
        assert!(
            analyze(&id_input).entities[0]
                .flows_via_node_config
                .is_empty()
        );
    }

    // --- other edges --------------------------------------------------------

    #[test]
    fn api_resources_name_own_resource_and_expanding_neighbours() {
        // line_items (from) --rel "order"--> orders (to). A change to `orders`
        // affects its own resource AND line_items?expand=order (which embeds it).
        let target = cat(
            &[("orders", "orders"), ("line_items", "lines")],
            &[("r_order", "order", "line_items", "orders")],
        );
        let input = ImpactInput {
            plan: &plan(&[("orders", Safety::Additive)]),
            current: Some(&target),
            target: &target,
            registrations: &[],
            flows: &[],
            suites: &[],
        };
        let e = &analyze(&input).entities[0];
        assert!(e.api_resources.contains(&"/api/rest/orders".to_string()));
        assert!(
            e.api_resources
                .contains(&"/api/rest/lines?expand=order".to_string()),
            "the neighbour resource embedding this entity is named: {:?}",
            e.api_resources
        );
    }

    #[test]
    fn suites_of_affected_flows_are_enumerated_across_versions() {
        let target = cat(&[("orders", "orders")], &[]);
        let input = ImpactInput {
            plan: &plan(&[("orders", Safety::Destructive)]),
            current: Some(&target),
            target: &target,
            registrations: &[reg("t1", "notify", "orders", "r1")],
            flows: &[],
            // notify has suites at v1 and v2; other has one that must NOT appear.
            suites: &[
                suite("t1", "notify", 1, "smoke"),
                suite("t1", "notify", 2, "regress"),
                suite("t1", "other", 1, "unrelated"),
            ],
        };
        let e = &analyze(&input).entities[0];
        assert_eq!(e.suites.len(), 2, "both versions of the affected flow");
        assert!(e.suites.iter().all(|s| s.flow_id == "notify"));
    }

    #[test]
    fn a_rename_surfaces_node_config_flows_by_the_old_name() {
        // orders renamed to orders2; the flow still references the OLD name, which
        // now dangles — it MUST surface (the not-rename-proof edge).
        let current = cat(&[("sales_orders", "orders")], &[]);
        let target = cat(&[("sales_orders", "orders2")], &[]);
        let input = ImpactInput {
            plan: &plan(&[("sales_orders", Safety::Destructive)]),
            current: Some(&current),
            target: &target,
            registrations: &[],
            flows: &[pg_flow("t1", "sync", 1, "orders")], // the OLD name
            suites: &[],
        };
        let e = &analyze(&input).entities[0];
        assert_eq!(e.change, EntityChangeKind::Changed);
        assert_eq!(e.entity_name, "orders2", "display name is the new name");
        assert_eq!(e.flows_via_node_config.len(), 1);
        assert_eq!(e.flows_via_node_config[0].referenced_name, "orders");
    }

    #[test]
    fn a_removed_entity_takes_its_api_resource_from_current() {
        let current = cat(&[("orders", "orders")], &[]);
        let target = cat(&[], &[]); // orders removed
        let input = ImpactInput {
            plan: &plan(&[("orders", Safety::Destructive)]),
            current: Some(&current),
            target: &target,
            registrations: &[],
            flows: &[],
            suites: &[],
        };
        let e = &analyze(&input).entities[0];
        assert_eq!(e.change, EntityChangeKind::Removed);
        assert_eq!(e.entity_name, "orders");
        assert_eq!(e.api_resources, vec!["/api/rest/orders".to_string()]);
    }

    #[test]
    fn empty_plan_is_a_clean_empty_report() {
        let target = cat(&[("orders", "orders")], &[]);
        let input = ImpactInput {
            plan: &plan(&[]),
            current: Some(&target),
            target: &target,
            registrations: &[reg("t1", "notify", "orders", "r1")],
            flows: &[],
            suites: &[],
        };
        let report = analyze(&input);
        assert!(report.is_empty());
        assert!(!report.requires_acknowledgement());
        assert_eq!(
            report.render(),
            "schema-change impact — no affected entities\n"
        );
    }

    // --- the 12g auto-run seam ----------------------------------------------

    /// An affected entity carrying `suites` — the only field the flattener reads.
    fn impacted(entity_id: &str, suites: &[SuiteEdge]) -> EntityImpact {
        EntityImpact {
            entity_id: entity_id.into(),
            entity_name: entity_id.into(),
            change: EntityChangeKind::Changed,
            destructive: false,
            flows_via_registration: Vec::new(),
            flows_via_node_config: Vec::new(),
            suites: suites.to_vec(),
            api_resources: Vec::new(),
        }
    }

    /// A `SuiteEdge` survives a JSON round trip with all FOUR identity fields
    /// intact — dropping any one of them (notably the version, which pins WHICH
    /// flow version the executor runs) silently mis-targets the auto-run.
    #[test]
    fn suite_edge_round_trips_through_json_keeping_every_identity_field() {
        let edge = suite("acme", "receiving", 7, "happy-path");
        let json = serde_json::to_string(&edge).expect("SuiteEdge serializes");
        assert_eq!(
            json,
            r#"{"tenant":"acme","flow_id":"receiving","flow_version":7,"suite_id":"happy-path"}"#,
        );
        let back: SuiteEdge = serde_json::from_str(&json).expect("SuiteEdge deserializes");
        assert_eq!(back, edge);
        assert_eq!(back.tenant, "acme");
        assert_eq!(back.flow_id, "receiving");
        assert_eq!(back.flow_version, 7);
        assert_eq!(back.suite_id, "happy-path");
    }

    /// The wire contract is field-for-field: a renamed (camelCase or shortened)
    /// field drops a REQUIRED field, and an extra field is refused outright, so
    /// the 12g input shape cannot drift silently.
    #[test]
    fn suite_edge_read_refuses_a_renamed_or_extra_field() {
        for wrong in [
            r#"{"tenant":"t","flow":"f","flow_version":1,"suite_id":"s"}"#,
            r#"{"tenant":"t","flow_id":"f","version":1,"suite_id":"s"}"#,
            r#"{"tenant":"t","flowId":"f","flowVersion":1,"suiteId":"s"}"#,
            r#"{"tenant":"t","flow_id":"f","suite_id":"s"}"#,
            r#"{"tenant":"t","flow_id":"f","flow_version":1,"suite_id":"s","extra":1}"#,
        ] {
            assert!(
                serde_json::from_str::<SuiteEdge>(wrong).is_err(),
                "a drifted suite-edge row must be refused: {wrong}",
            );
        }
    }

    /// The emitted array is de-duplicated (one suite reached through two affected
    /// entities appears ONCE) and ordered by `(tenant, flow_id, flow_version,
    /// suite_id)`, so repeated runs are byte-identical.
    #[test]
    fn suite_selectors_dedupe_and_order_by_tenant_flow_version_suite() {
        let shared = suite("acme", "receiving", 1, "happy");
        let report = ImpactReport {
            entities: vec![
                impacted(
                    "e1",
                    &[
                        suite("acme", "receiving", 2, "happy"),
                        shared.clone(),
                        suite("zeta", "audit", 1, "a"),
                    ],
                ),
                impacted(
                    "e2",
                    &[
                        shared.clone(),
                        suite("acme", "billing", 1, "b"),
                        suite("acme", "receiving", 1, "aardvark"),
                    ],
                ),
            ],
        };

        let selectors = suite_selectors(&report).expect("complete edges flatten");
        assert_eq!(
            selectors,
            vec![
                suite("acme", "billing", 1, "b"),
                suite("acme", "receiving", 1, "aardvark"),
                suite("acme", "receiving", 1, "happy"),
                suite("acme", "receiving", 2, "happy"),
                suite("zeta", "audit", 1, "a"),
            ],
            "selectors sort by (tenant, flow_id, flow_version, suite_id) and dedupe",
        );
        assert_eq!(
            suite_selectors(&report).expect("second run flattens"),
            selectors,
            "the flattening is deterministic across runs",
        );
    }

    /// An incomplete edge REFUSES the whole flattening — it is never dropped,
    /// because a dropped suite is one the auto-run seam would never execute yet
    /// still report as clean. Every defect is named, in emit order.
    #[test]
    fn suite_selectors_refuse_incomplete_edges_instead_of_dropping_them() {
        for (bad, defect) in [
            (suite("", "receiving", 1, "s"), SuiteEdgeDefect::EmptyTenant),
            (suite("acme", "", 1, "s"), SuiteEdgeDefect::EmptyFlowId),
            (
                suite("acme", "receiving", 1, ""),
                SuiteEdgeDefect::EmptySuiteId,
            ),
            (
                suite("acme", "receiving", -1, "s"),
                SuiteEdgeDefect::NegativeFlowVersion,
            ),
        ] {
            let good = suite("acme", "receiving", 1, "keeper");
            let report = ImpactReport {
                entities: vec![impacted("e1", &[good.clone(), bad.clone()])],
            };
            let err = suite_selectors(&report)
                .expect_err("an incomplete suite edge must refuse the flattening");
            assert_eq!(
                err.rejected,
                vec![(bad.clone(), defect)],
                "the refusal names the offending edge and its defect",
            );
            assert!(
                err.to_string().contains(defect.as_str()),
                "the refusal renders the defect: {err}",
            );
            assert!(
                suite_selectors_json(&report).is_err(),
                "the JSON emitter refuses the same report",
            );
        }
    }

    /// The exact bytes the executor reads from `--impact-report <path>`: a JSON
    /// array of `{tenant, flow_id, flow_version, suite_id}` objects in selector
    /// order. Frozen — this is the wamn-12g wire contract.
    #[test]
    fn suite_selectors_json_emits_the_executor_contract_array() {
        let report = ImpactReport {
            entities: vec![impacted(
                "e1",
                &[
                    suite("acme", "receiving", 2, "second"),
                    suite("acme", "receiving", 1, "first"),
                ],
            )],
        };
        assert_eq!(
            suite_selectors_json(&report).expect("complete edges emit"),
            r#"[
  {
    "tenant": "acme",
    "flow_id": "receiving",
    "flow_version": 1,
    "suite_id": "first"
  },
  {
    "tenant": "acme",
    "flow_id": "receiving",
    "flow_version": 2,
    "suite_id": "second"
  }
]"#,
        );
    }

    /// A report with no affected entities (or no suites) emits an empty array,
    /// not a refusal and not rendered text — the auto-run seam sees "nothing to
    /// run" as data.
    #[test]
    fn suite_selectors_json_emits_an_empty_array_for_no_suites() {
        assert_eq!(
            suite_selectors_json(&ImpactReport::default()).expect("an empty report emits"),
            "[]",
        );
        let no_suites = ImpactReport {
            entities: vec![impacted("e1", &[])],
        };
        assert_eq!(
            suite_selectors_json(&no_suites).expect("a suite-less report emits"),
            "[]",
        );
    }
}
