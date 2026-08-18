//! wamn schema-change impact analysis (11.8).
//!
//! The PURE decision behind the operations-only `wamn-ctl impact-report`: given
//! a compiled [`MigrationPlan`]
//! and the dependency data a control-plane verb reads from the project database,
//! enumerate — per affected entity — WHAT changes and WHAT downstream depends on
//! it, so a schema designer sees the blast radius *before* any DDL applies.
//!
//! This crate is a JOIN over data the platform already stores; it holds no
//! connection, clock, or wasm. The [`analyze`] inputs are plain data ([`ImpactInput`]);
//! the [`ImpactReport`] output is plain data. The three edges it computes:
//!
//! 1. **affected entity + classification** — group the plan's operations by
//!    [`wamn_schema_compiler::Operation::entity`]; an entity is destructive iff any of its ops
//!    is [`wamn_schema_compiler::Safety::Destructive`] (the plan is the authoritative source —
//!    no SQL re-parse).
//! 2. **flows via event registration** — id-keyed and rename-proof: registrations
//!    whose stable `entity_id` equals the affected entity's id
//!    (`catalog.event_registrations`, the `event_registrations_by_entity` index).
//! 3. **generated-API resources** — pure over the catalog: the entity's own
//!    `/api/rest/{name}` plus the neighbours' `?expand=` resources that embed it.

use std::collections::{BTreeMap, BTreeSet};

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

/// The pure analysis inputs. `current` is the pre-migration applied catalog (the
/// diff/plan source; `None` for a first materialization); `target` is the
/// post-migration catalog. `registrations` is read cross-tenant by the superuser
/// driver.
#[derive(Debug, Clone)]
pub struct ImpactInput<'a> {
    pub plan: &'a MigrationPlan,
    pub current: Option<&'a Catalog>,
    pub target: &'a Catalog,
    pub registrations: &'a [RegistrationEdge],
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
    /// The generated-API resources over the catalog: `/api/rest/{name}` plus the
    /// neighbours' `?expand=` resources that embed this entity.
    pub api_resources: Vec<String>,
}

impl EntityImpact {
    /// `true` if some flow depends on this entity. The generated-API resources
    /// are pure catalog derivations, NOT downstream dependents — every entity has
    /// them — so they do not count here.
    pub fn has_downstream_impact(&self) -> bool {
        !self.flows_via_registration.is_empty()
    }
}

/// The whole impact report — the affected entities, entity-id ordered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImpactReport {
    pub entities: Vec<EntityImpact>,
}

impl ImpactReport {
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// `true` if any affected entity is destructive.
    pub fn any_destructive(&self) -> bool {
        self.entities.iter().any(|e| e.destructive)
    }

    /// A human-readable rendering: the single review surface a schema designer
    /// reads before operations reconciliation.
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
            if !e.has_downstream_impact() {
                out.push_str("      (no dependent flows)\n");
            }
        }
        out
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
        *d |= op.safety().is_destructive();
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

        // Edge 3: generated-API resources (pure over the catalog holding the
        // entity — target, or current for a removed entity).
        let api_resources = api_resources_for(id, input.target, input.current);

        entities.push(EntityImpact {
            entity_id: id.to_string(),
            entity_name,
            change,
            destructive,
            flows_via_registration,
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
                .map(|(entity, safety)| {
                    Operation::classified(format!("op on {entity}"), *safety, (*entity).to_string())
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
    /// flow remains classified as destructive and retains that edge.
    #[test]
    fn destructive_change_with_impact_keeps_both_facts() {
        let target = cat(&[("orders", "orders")], &[]);
        let input = ImpactInput {
            plan: &plan(&[("orders", Safety::Destructive)]),
            current: Some(&target),
            target: &target,
            registrations: &[reg("t1", "notify", "orders", "r1")],
        };
        let report = analyze(&input);
        assert!(report.any_destructive());
        assert!(report.entities[0].has_downstream_impact());
        assert_eq!(report.entities[0].entity_name, "orders");
    }

    /// MUTANT 2 negative: destructiveness and dependency edges stay independent.
    #[test]
    fn destructiveness_and_dependencies_stay_independent() {
        let target = cat(&[("orders", "orders"), ("audit", "audit")], &[]);
        // orders: destructive but no dependents. audit: additive with a dependent.
        let input = ImpactInput {
            plan: &plan(&[("orders", Safety::Destructive), ("audit", Safety::Additive)]),
            current: Some(&target),
            target: &target,
            registrations: &[reg("t1", "log", "audit", "r-audit")],
        };
        let report = analyze(&input);
        assert!(report.any_destructive());
        let orders = report
            .entities
            .iter()
            .find(|e| e.entity_id == "orders")
            .unwrap();
        let audit = report
            .entities
            .iter()
            .find(|e| e.entity_id == "audit")
            .unwrap();
        assert!(orders.destructive);
        assert!(!orders.has_downstream_impact());
        assert!(!audit.destructive);
        assert!(audit.has_downstream_impact());
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
    fn a_rename_reports_the_new_display_name() {
        // orders renamed to orders2 (id `sales_orders` kept across both versions).
        let current = cat(&[("sales_orders", "orders")], &[]);
        let target = cat(&[("sales_orders", "orders2")], &[]);
        let input = ImpactInput {
            plan: &plan(&[("sales_orders", Safety::Destructive)]),
            current: Some(&current),
            target: &target,
            registrations: &[],
        };
        let e = &analyze(&input).entities[0];
        assert_eq!(e.change, EntityChangeKind::Changed);
        assert_eq!(e.entity_name, "orders2", "display name is the new name");
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
        };
        let report = analyze(&input);
        assert!(report.is_empty());
        assert_eq!(
            report.render(),
            "schema-change impact — no affected entities\n"
        );
    }
}
