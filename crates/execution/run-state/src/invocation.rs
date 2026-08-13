//! Durable queries owned by the production flow-invocation service.
//!
//! Resolution deliberately includes disabled definitions. Authentication is an
//! adapter concern, but the selected source document is returned before the
//! admissions ledger can be queried, preserving FLOW-SPEC rev18 §6.2's order.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct InvocationTarget {
    pub catalog_version: i32,
    pub definition_hash: String,
    pub flow_id: String,
    pub flow_version: i32,
    pub definition: Value,
    pub auth_policy: Value,
    pub enabled: bool,
    pub idempotency_required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InvocationOutcome {
    pub run_id: String,
    pub kind: String,
    pub body: Value,
    pub http_status: Option<u16>,
    pub hash: String,
    pub flow_id: String,
    pub flow_version: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InvocationRecovery {
    Missing,
    InFlight { run_id: String },
    Released(InvocationOutcome),
    IdempotencyKeyReused,
    IdempotencyScopeChanged,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InvocationPoll {
    Running,
    Released(InvocationOutcome),
    NotFound,
}

/// Resolve an HTTP attachment from the applied release, even when disabled.
///
/// Params: catalog id, environment, attachment id. Tombstoned/removed routes
/// return no row and therefore end transparent recovery.
pub fn resolve_invocation_target_sql() -> &'static str {
    "\
SELECT h.applied_catalog_version, a.definition_hash, a.flow_id, f.flow_version, \
       a.definition_json::text, s.definition_json::text, \
       COALESCE(x.enabled AND x.confirmed_definition_hash = a.definition_hash, false), \
       f.execution_bundle_hash, b.exact_bytes \
  FROM catalog.catalog_heads AS h \
  JOIN catalog.release_attachments AS a \
    ON a.tenant_id = h.tenant_id AND a.catalog_id = h.catalog_id \
   AND a.catalog_version = h.applied_catalog_version \
  JOIN catalog.release_flows AS f \
    ON f.tenant_id = a.tenant_id AND f.catalog_id = a.catalog_id \
   AND f.catalog_version = a.catalog_version AND f.flow_id = a.flow_id \
  LEFT JOIN catalog.execution_bundles AS b \
    ON b.tenant_id = f.tenant_id \
   AND b.execution_bundle_hash = f.execution_bundle_hash \
  JOIN catalog.release_sources AS s \
    ON s.tenant_id = a.tenant_id AND s.catalog_id = a.catalog_id \
   AND s.catalog_version = a.catalog_version AND s.source_id = a.source_id \
  LEFT JOIN catalog.attachment_activation AS x \
    ON x.tenant_id = h.tenant_id AND x.catalog_id = h.catalog_id \
   AND x.environment = h.environment AND x.attachment_id = a.attachment_id \
 WHERE h.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND h.catalog_id = $1 AND h.environment = $2 AND a.attachment_id = $3 \
   AND a.attachment_kind IN ('http', 'studio') \
   AND NOT EXISTS ( \
       SELECT 1 FROM catalog.attachment_tombstones AS dead \
        WHERE dead.tenant_id = h.tenant_id AND dead.catalog_id = h.catalog_id \
          AND dead.environment = h.environment \
          AND dead.attachment_id = a.attachment_id \
   )"
}

/// Derive the caller-key requirement from one flow's exact own plan.
///
/// A malformed or mismatched plan fails closed here; claim-time plan validation
/// remains the authority that refuses execution of those bytes.
pub fn invocation_idempotency_required(execution_bundle_hash: &str, exact_bytes: &[u8]) -> bool {
    match wamn_catalog::read_execution_plan(execution_bundle_hash, exact_bytes) {
        Ok(plan) => plan.requires_idempotency_key(),
        Err(_) => true,
    }
}

/// Look up the admissions ledger after route-policy authentication.
///
/// Params: catalog id, environment, attachment id, principal digest,
/// client-key digest, current definition hash, request fingerprint.
pub fn lookup_invocation_recovery_sql() -> &'static str {
    "\
SELECT CASE \
         WHEN a.run_id IS NULL THEN 'missing' \
         WHEN a.definition_hash <> $6 THEN 'idempotency-scope-changed' \
         WHEN a.client_request_fingerprint <> $7 THEN 'idempotency-key-reused' \
         WHEN r.caller_released_at IS NULL THEN 'in-flight' \
         ELSE 'released' \
       END AS result_code, \
       a.run_id, r.caller_outcome_kind, r.caller_outcome_json::text, \
       r.caller_http_status, r.caller_outcome_hash, r.flow_id, r.flow_version \
  FROM (SELECT 1) AS one \
  LEFT JOIN wamn_run.invocation_admissions AS a \
    ON a.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND a.catalog_id = $1 AND a.environment = $2 AND a.attachment_id = $3 \
   AND a.principal_digest = $4 AND a.client_key_digest = $5 \
  LEFT JOIN wamn_run.runs AS r \
    ON r.tenant_id = a.tenant_id AND r.run_id = a.run_id"
}

pub fn poll_invocation_outcome_sql() -> &'static str {
    "\
SELECT CASE WHEN r.run_id IS NULL THEN 'not-found' \
            WHEN r.caller_released_at IS NULL THEN 'running' \
            ELSE 'released' END AS result_code, \
       r.run_id, r.caller_outcome_kind, r.caller_outcome_json::text, \
       r.caller_http_status, r.caller_outcome_hash, r.flow_id, r.flow_version \
  FROM (SELECT 1) AS one \
  LEFT JOIN wamn_run.runs AS r \
    ON r.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND r.run_id = $1"
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_catalog::{
        CALLABLE_CONTRACT_VERSION, CallableContract, CallableEffectCeiling, CallableReturnContract,
        ExecutionEffectPolicy, ExecutionPlanBody, ExecutionPlanEdge, ExecutionPlanNode,
        ExecutionPlanV2, ExecutionRuntimeRevision, ExecutionSourceMapEntry,
        HOST_EFFECT_CONTRACT_VERSION, RootTerminalBehavior, entry_input_schema_hash,
        execution_bundle_hash,
    };

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn invocation_plan(extra: Option<(&str, ExecutionEffectPolicy)>) -> (String, Vec<u8>) {
        let guard = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object"
        });
        let mut nodes = vec![ExecutionPlanNode {
            local_node_id: "request".parse().unwrap(),
            source_node_id: "request".into(),
            node_type: "request".into(),
            config: serde_json::json!({"input-schema": guard}),
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        }];
        if let Some((node_type, effect_policy)) = extra {
            nodes.push(ExecutionPlanNode {
                local_node_id: "work".parse().unwrap(),
                source_node_id: "work".into(),
                node_type: node_type.into(),
                config: if node_type == "call-flow" {
                    serde_json::json!({"site": "work", "flow-id": "callee"})
                } else {
                    serde_json::json!({})
                },
                effect_policy,
                source_connection_requirement: None,
            });
        }
        nodes.push(ExecutionPlanNode {
            local_node_id: "respond".parse().unwrap(),
            source_node_id: "respond".into(),
            node_type: "respond".into(),
            config: serde_json::json!({"status": 200}),
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        });
        let path = if extra.is_some() {
            ["request", "work", "respond"].as_slice()
        } else {
            ["request", "respond"].as_slice()
        };
        let edges = path
            .windows(2)
            .map(|pair| ExecutionPlanEdge {
                source: pair[0].parse().unwrap(),
                source_port: "main".into(),
                destination: pair[1].parse().unwrap(),
                destination_port: None,
                fan_out_ordinal: 0,
            })
            .collect();
        let source_map = nodes
            .iter()
            .map(|node| ExecutionSourceMapEntry {
                local_node_id: node.local_node_id.clone(),
                source_node_id: node.source_node_id.clone(),
            })
            .collect();
        let plan = ExecutionPlanV2::new(
            ExecutionRuntimeRevision {
                flowrunner_component_digest: DIGEST_A.into(),
                effect_provider_revision: DIGEST_B.into(),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
            },
            DIGEST_A,
            ExecutionPlanBody {
                entry_instruction: "request".parse().unwrap(),
                nodes,
                edges,
                root_terminal_behavior: RootTerminalBehavior::Respond {
                    responders: vec!["respond".parse().unwrap()],
                },
                entry_input_schema_guard: guard.clone(),
                callable_contract: Some(CallableContract {
                    version: CALLABLE_CONTRACT_VERSION.into(),
                    input_schema_hash: entry_input_schema_hash(&guard),
                    return_contract: CallableReturnContract::UntypedJsonBody,
                    effect_ceiling: CallableEffectCeiling::Effectful,
                }),
                source_map,
            },
        )
        .unwrap();
        let exact_bytes = serde_json::to_vec(&plan).unwrap();
        (execution_bundle_hash(&exact_bytes), exact_bytes)
    }

    #[test]
    fn lookup_order_resolves_disabled_policy_before_the_ledger() {
        let resolve = resolve_invocation_target_sql();
        assert!(resolve.contains("release_sources AS s"));
        assert!(resolve.contains("LEFT JOIN catalog.attachment_activation"));
        assert!(!resolve.contains("WHERE x.enabled"));
        assert!(resolve.contains("attachment_tombstones"));
        assert!(resolve.contains("LEFT JOIN catalog.execution_bundles AS b"));
        assert!(resolve.contains("f.execution_bundle_hash, b.exact_bytes"));

        let recovery = lookup_invocation_recovery_sql();
        let scope = recovery.find("a.definition_hash <> $6").unwrap();
        let body = recovery.find("a.client_request_fingerprint <> $7").unwrap();
        assert!(scope < body);
        assert!(!recovery.contains("expires_at"));
        assert!(!recovery.contains("outcome-expired"));
    }

    #[test]
    fn malformed_own_plan_requires_a_key_fail_closed() {
        let bytes = b"{}";
        assert!(invocation_idempotency_required(
            &wamn_catalog::execution_bundle_hash(bytes),
            bytes
        ));
    }

    #[test]
    fn own_plan_requires_a_key_for_effectful_or_call_flow_only() {
        for (extra, expected) in [
            (None, false),
            (Some(("custom", ExecutionEffectPolicy::Pure)), false),
            (Some(("custom", ExecutionEffectPolicy::Effectful)), true),
            (Some(("call-flow", ExecutionEffectPolicy::Effectful)), true),
        ] {
            let (hash, bytes) = invocation_plan(extra);
            assert_eq!(
                invocation_idempotency_required(&hash, &bytes),
                expected,
                "plan extra {extra:?}"
            );
        }
    }

    #[test]
    fn polling_returns_only_exact_stored_columns() {
        let sql = poll_invocation_outcome_sql();
        for column in [
            "caller_outcome_kind",
            "caller_outcome_json::text",
            "caller_http_status",
            "caller_outcome_hash",
            "flow_id",
            "flow_version",
        ] {
            assert!(sql.contains(column), "missing {column}");
        }
        assert!(!sql.contains("COALESCE(r.caller_http_status"));
    }
}
