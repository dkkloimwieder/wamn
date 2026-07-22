//! The name-keyed node-config edge (11.8): which node of a flow structurally
//! references a catalog entity BY NAME.
//!
//! A flow's `postgres` node names the entity it reads/writes in its opaque
//! `config["entity"]`, which the generated REST router resolves **by entity name**
//! (`/api/rest/{entity.name}`, `wamn_api` `entity_by_name`). This is the
//! **NOT-rename-proof** edge: a field/entity rename changes `entity.name` and
//! silently dangles the ref — unlike an event registration, which keys on the
//! stable entity id (`wamn_event_reg`, rename-proof). Surfacing it is the whole
//! point of the edge: a dangling `config["entity"]` after a rename is a genuine
//! report line, not an error the analysis can fix.
//!
//! `wamn_flow` types the config as an opaque `serde_json::Value`, so this is a
//! JSON lookup, not a typed field access.

use wamn_flow::Node;

/// The node types whose `config["entity"]` names a catalog entity by its logical
/// NAME. `postgres` (the CRUD node) uses it as `/api/rest/{entity}`;
/// `postgres-query` is author-written raw SQL that references tables textually and
/// carries no structured `entity` key today — it is listed for forward-safety, so
/// a future query node that adopts an `entity` hint is covered without a code
/// change (a config without the key simply never matches).
pub const ENTITY_NAME_CONFIG_NODE_TYPES: &[&str] = &["postgres", "postgres-query"];

/// The catalog entity NAME a node references through its config, if any. Returns
/// `None` for a node type that carries no entity-name config key or whose
/// `config["entity"]` is absent / not a string.
pub fn node_entity_name(node: &Node) -> Option<&str> {
    if !ENTITY_NAME_CONFIG_NODE_TYPES.contains(&node.node_type.as_str()) {
        return None;
    }
    node.config.get("entity")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_flow::Flow;

    fn flow_with_node(node_type: &str, config: serde_json::Value) -> Flow {
        let json = serde_json::json!({
            "schema-version": "0.1",
            "flow-id": "f",
            "version": 1,
            "trigger": { "type": "manual" },
            "entry": "n",
            "nodes": [ { "id": "n", "type": node_type, "config": config } ],
        });
        Flow::from_json(&json.to_string()).expect("valid flow fixture")
    }

    #[test]
    fn postgres_node_yields_its_config_entity_name() {
        let f = flow_with_node(
            "postgres",
            serde_json::json!({ "entity": "orders", "op": "get" }),
        );
        assert_eq!(node_entity_name(&f.nodes[0]), Some("orders"));
    }

    #[test]
    fn a_non_postgres_node_is_never_an_entity_ref() {
        let f = flow_with_node("transform", serde_json::json!({ "entity": "orders" }));
        assert_eq!(node_entity_name(&f.nodes[0]), None);
    }

    #[test]
    fn a_postgres_node_without_an_entity_key_yields_none() {
        let f = flow_with_node("postgres-query", serde_json::json!({ "sql": "SELECT 1" }));
        assert_eq!(node_entity_name(&f.nodes[0]), None);
    }
}
