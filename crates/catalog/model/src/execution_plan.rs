//! Exact-byte executable-plan wire and semantic validation.

use std::collections::BTreeSet;

use boon::{Compiler, Draft, Schemas};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use wamn_flow::node_contract::ConnectionTypeDescriptor;

use crate::{CatalogIdentityError, ExecutionNodeId, digest, validate_digest};

/// The only execution-plan format shipped by the MVP.
pub const EXECUTION_PLAN_FORMAT_VERSION: &str = "0.1";
/// The only plan compiler revision shipped by the MVP.
pub const PLAN_COMPILER_REVISION: &str = "0.1";
/// The only host-effect contract understood by execution plan format 0.1.
pub const HOST_EFFECT_CONTRACT_VERSION: &str = "0.1";
/// The only callable-contract version shipped by the MVP.
pub const CALLABLE_CONTRACT_VERSION: &str = "0.1";

const ENTRY_SCHEMA_URI: &str = "mem://execution-entry-input-schema.json";
const JSON_SCHEMA_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Exact executable revision required by one plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionRuntimeRevision {
    pub flowrunner_component_digest: String,
    pub effect_provider_revision: String,
    pub host_effect_contract_version: String,
}

/// The immutable plan header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanHeader {
    pub format_version: String,
    pub plan_compiler_revision: String,
    pub runtime_revision: ExecutionRuntimeRevision,
    pub root_artifact_hash: String,
}

/// Whether a plan node may produce an externally observable effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionEffectPolicy {
    Pure,
    Effectful,
}

/// One artifact-local connection requirement resolved into the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionConnectionRequirement {
    pub name: String,
    pub descriptor: ConnectionTypeDescriptor,
}

/// The complete durable config for an ordinary `call-flow` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CallFlowInstruction {
    pub site: ExecutionNodeId,
    pub flow_id: String,
}

/// One executable node in its owning flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanNode {
    pub local_node_id: ExecutionNodeId,
    pub source_node_id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub config: Value,
    pub effect_policy: ExecutionEffectPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_connection_requirement: Option<ExecutionConnectionRequirement>,
}

/// One port-qualified plan edge with explicit fan-out order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanEdge {
    pub source: ExecutionNodeId,
    pub source_port: String,
    pub destination: ExecutionNodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_port: Option<String>,
    pub fan_out_ordinal: u32,
}

/// How successful completion of the root graph reaches its caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum RootTerminalBehavior {
    Respond { responders: Vec<ExecutionNodeId> },
    FrontierExhaustion,
}

/// The only response body contract shipped by the MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CallableReturnContract {
    UntypedJsonBody,
}

/// The only callable effect ceiling shipped by the MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CallableEffectCeiling {
    Effectful,
}

/// The intrinsic callable boundary recorded by one own-flow plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CallableContract {
    pub version: String,
    pub input_schema_hash: String,
    pub return_contract: CallableReturnContract,
    pub effect_ceiling: CallableEffectCeiling,
}

/// Source coordinates for one own-flow node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionSourceMapEntry {
    pub local_node_id: ExecutionNodeId,
    pub source_node_id: String,
}

/// The complete executable body of one validated flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanBody {
    pub entry_instruction: ExecutionNodeId,
    pub nodes: Vec<ExecutionPlanNode>,
    pub edges: Vec<ExecutionPlanEdge>,
    pub root_terminal_behavior: RootTerminalBehavior,
    pub entry_input_schema_guard: Value,
    #[serde(deserialize_with = "deserialize_required_option")]
    #[schemars(required)]
    pub callable_contract: Option<CallableContract>,
    pub source_map: Vec<ExecutionSourceMapEntry>,
}

/// The sole executable representation stored for a validated flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanV2 {
    pub header: ExecutionPlanHeader,
    pub body: ExecutionPlanBody,
}

impl ExecutionPlanV2 {
    /// Construct and validate one complete own-flow plan without normalizing it.
    pub fn new(
        runtime_revision: ExecutionRuntimeRevision,
        root_artifact_hash: impl Into<String>,
        body: ExecutionPlanBody,
    ) -> Result<Self, CatalogIdentityError> {
        let plan = Self {
            header: ExecutionPlanHeader {
                format_version: EXECUTION_PLAN_FORMAT_VERSION.to_string(),
                plan_compiler_revision: PLAN_COMPILER_REVISION.to_string(),
                runtime_revision,
                root_artifact_hash: root_artifact_hash.into(),
            },
            body,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub(crate) fn validate(&self) -> Result<(), CatalogIdentityError> {
        self.validate_header()?;
        if self.body.nodes.is_empty() {
            return invalid("execution plan must contain at least one node");
        }

        let node_ids = self
            .body
            .nodes
            .iter()
            .map(|node| &node.local_node_id)
            .collect::<BTreeSet<_>>();
        if node_ids.len() != self.body.nodes.len() {
            return invalid("execution plan local node ids must be unique");
        }
        if !node_ids.contains(&self.body.entry_instruction) {
            return invalid("execution plan entry instruction must name a plan node");
        }

        let entry = self
            .node_for(&self.body.entry_instruction)
            .expect("execution plan entry membership checked");
        if !matches!(entry.node_type.as_str(), "request" | "event") {
            return invalid("execution plan entry must be request or event");
        }

        for node in &self.body.nodes {
            self.validate_node(node)?;
        }
        self.validate_edges(&node_ids)?;

        if self
            .body
            .edges
            .iter()
            .any(|edge| edge.destination == self.body.entry_instruction)
        {
            return invalid("execution plan entry instruction cannot have an incoming edge");
        }
        if reachable_from(
            &self.body.entry_instruction,
            &self.body.nodes,
            &self.body.edges,
            false,
        )
        .len()
            != self.body.nodes.len()
        {
            return invalid("every execution plan node must be reachable from the entry");
        }

        self.validate_source_map(&node_ids)?;
        self.validate_terminal_behavior(entry)?;
        self.validate_entry_guard(entry)?;
        self.validate_callable_contract(entry)?;
        Ok(())
    }

    /// Whether admitting this plan needs a caller-supplied idempotency key.
    pub fn requires_idempotency_key(&self) -> bool {
        self.body.nodes.iter().any(|node| {
            node.effect_policy == ExecutionEffectPolicy::Effectful || node.node_type == "call-flow"
        })
    }

    /// Derive the callable contract implied by the plan's intrinsic graph shape.
    pub fn derived_callable_contract(
        &self,
    ) -> Result<Option<CallableContract>, CatalogIdentityError> {
        let entry = self.node_for(&self.body.entry_instruction).ok_or_else(|| {
            CatalogIdentityError::InvalidDefinition {
                message: "execution plan entry instruction must name a plan node".to_string(),
            }
        })?;
        Ok(self
            .is_intrinsically_callable(entry)
            .then(|| CallableContract {
                version: CALLABLE_CONTRACT_VERSION.to_string(),
                input_schema_hash: entry_input_schema_hash(&self.body.entry_input_schema_guard),
                return_contract: CallableReturnContract::UntypedJsonBody,
                effect_ceiling: CallableEffectCeiling::Effectful,
            }))
    }

    fn validate_header(&self) -> Result<(), CatalogIdentityError> {
        if self.header.format_version != EXECUTION_PLAN_FORMAT_VERSION {
            return invalid("execution plan format-version must be textual 0.1");
        }
        if self.header.plan_compiler_revision != PLAN_COMPILER_REVISION {
            return invalid("plan-compiler-revision must be textual 0.1");
        }
        validate_digest(
            &self.header.runtime_revision.flowrunner_component_digest,
            "flowrunner-component-digest",
        )?;
        validate_digest(
            &self.header.runtime_revision.effect_provider_revision,
            "effect-provider-revision",
        )?;
        validate_digest(&self.header.root_artifact_hash, "root-artifact-hash")?;
        if self.header.runtime_revision.host_effect_contract_version != HOST_EFFECT_CONTRACT_VERSION
        {
            return invalid("host-effect-contract-version must be textual 0.1");
        }
        Ok(())
    }

    fn validate_node(&self, node: &ExecutionPlanNode) -> Result<(), CatalogIdentityError> {
        if node.source_node_id.is_empty() || node.node_type.is_empty() {
            return invalid("execution plan source node id and type must not be empty");
        }
        if let Some(requirement) = &node.source_connection_requirement {
            if requirement.name.is_empty()
                || !requirement
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return invalid("execution plan connection requirement name must be a slug");
            }
            if requirement.descriptor != ConnectionTypeDescriptor::http_v1() {
                return invalid("execution plan connection descriptor must be canonical HTTP v1");
            }
        }

        if node.node_type == "call-flow" {
            let instruction = serde_json::from_value::<CallFlowInstruction>(node.config.clone())
                .map_err(|error| CatalogIdentityError::InvalidDefinition {
                    message: format!("call-flow config is invalid: {error}"),
                })?;
            if instruction.site != node.local_node_id {
                return invalid("call-flow site must equal its local-node-id");
            }
            if !valid_flow_slug(&instruction.flow_id) {
                return invalid("call-flow flow-id must be a lowercase slug");
            }
            if node.effect_policy != ExecutionEffectPolicy::Effectful {
                return invalid("call-flow nodes must be effectful");
            }
            if node.source_connection_requirement.is_some() {
                return invalid("call-flow nodes cannot require a connection");
            }
        }
        Ok(())
    }

    fn validate_edges(
        &self,
        node_ids: &BTreeSet<&ExecutionNodeId>,
    ) -> Result<(), CatalogIdentityError> {
        let mut fan_out = BTreeSet::new();
        for edge in &self.body.edges {
            if !node_ids.contains(&edge.source) || !node_ids.contains(&edge.destination) {
                return invalid("execution plan edges must name existing nodes");
            }
            if edge.source_port.is_empty()
                || edge.destination_port.as_deref().is_some_and(str::is_empty)
                || !fan_out.insert((
                    &edge.source,
                    edge.source_port.as_str(),
                    edge.fan_out_ordinal,
                ))
            {
                return invalid(
                    "execution plan edge ports and fan-out ordinals must be unambiguous",
                );
            }
        }
        Ok(())
    }

    fn validate_source_map(
        &self,
        node_ids: &BTreeSet<&ExecutionNodeId>,
    ) -> Result<(), CatalogIdentityError> {
        let mut mapped = BTreeSet::new();
        for source in &self.body.source_map {
            if source.source_node_id.is_empty()
                || !node_ids.contains(&source.local_node_id)
                || !mapped.insert(&source.local_node_id)
            {
                return invalid("execution source-map must name every local node exactly once");
            }
            let node = self
                .node_for(&source.local_node_id)
                .expect("source-map local node membership checked");
            if source.source_node_id != node.source_node_id {
                return invalid("execution source-map entry must match its node source");
            }
        }
        if mapped != *node_ids {
            return invalid("execution source-map must cover every plan node exactly once");
        }
        Ok(())
    }

    fn validate_terminal_behavior(
        &self,
        entry: &ExecutionPlanNode,
    ) -> Result<(), CatalogIdentityError> {
        match (&entry.node_type[..], &self.body.root_terminal_behavior) {
            ("request", RootTerminalBehavior::Respond { responders }) => {
                let responder_ids = responders.iter().collect::<BTreeSet<_>>();
                if responders.is_empty() || responder_ids.len() != responders.len() {
                    return invalid("request root responders must be nonempty and unique");
                }
                for responder_id in responders {
                    let Some(responder) = self.node_for(responder_id) else {
                        return invalid("execution plan root terminal must name existing nodes");
                    };
                    if responder.node_type != "respond" {
                        return invalid("root responders must name respond nodes");
                    }
                }
                let complete = self
                    .body
                    .nodes
                    .iter()
                    .filter(|node| node.node_type == "respond")
                    .map(|node| &node.local_node_id)
                    .collect::<BTreeSet<_>>();
                if responder_ids != complete {
                    return invalid("request root responders must name every respond node");
                }
            }
            ("request", RootTerminalBehavior::FrontierExhaustion) => {
                return invalid("request-entry plans require root responders");
            }
            (_, RootTerminalBehavior::Respond { .. }) => {
                return invalid("only request-entry plans may name root responders");
            }
            (_, RootTerminalBehavior::FrontierExhaustion) => {
                if self
                    .body
                    .nodes
                    .iter()
                    .any(|node| node.node_type == "respond")
                {
                    return invalid("event plans cannot contain respond nodes");
                }
            }
        }
        Ok(())
    }

    fn validate_entry_guard(&self, entry: &ExecutionPlanNode) -> Result<(), CatalogIdentityError> {
        if entry.node_type == "request" {
            let guard = entry.config.get("input-schema").ok_or_else(|| {
                CatalogIdentityError::InvalidDefinition {
                    message: "request entry config must contain input-schema".to_string(),
                }
            })?;
            if guard != &self.body.entry_input_schema_guard {
                return invalid("entry input-schema guard must match request config");
            }
        } else if self.body.entry_input_schema_guard != Value::Bool(true) {
            return invalid("event entry input-schema guard must be true");
        }
        validate_entry_schema(&self.body.entry_input_schema_guard)
    }

    fn validate_callable_contract(
        &self,
        entry: &ExecutionPlanNode,
    ) -> Result<(), CatalogIdentityError> {
        let callable = self.is_intrinsically_callable(entry);
        if callable != self.body.callable_contract.is_some() {
            return invalid("callable-contract presence must match intrinsic callability");
        }
        if let Some(contract) = &self.body.callable_contract {
            if contract.version != CALLABLE_CONTRACT_VERSION {
                return invalid("callable contract version must be textual 0.1");
            }
            validate_digest(&contract.input_schema_hash, "input-schema-hash")?;
            if contract.input_schema_hash
                != entry_input_schema_hash(&self.body.entry_input_schema_guard)
            {
                return invalid("callable input-schema-hash must match the entry guard");
            }
        }
        Ok(())
    }

    fn is_intrinsically_callable(&self, entry: &ExecutionPlanNode) -> bool {
        let RootTerminalBehavior::Respond { responders } = &self.body.root_terminal_behavior else {
            return false;
        };
        if entry.node_type != "request"
            || responders
                .iter()
                .any(|responder| self.body.edges.iter().any(|edge| edge.source == *responder))
        {
            return false;
        }

        let region = reachable_from(
            &self.body.entry_instruction,
            &self.body.nodes,
            &self.body.edges,
            true,
        );
        let stoppers = region
            .iter()
            .filter(|node_id| {
                self.node_for(node_id)
                    .is_some_and(|node| matches!(node.node_type.as_str(), "respond" | "fail"))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if stoppers.iter().all(|node_id| {
            self.node_for(node_id)
                .is_none_or(|node| node.node_type != "respond")
        }) {
            return false;
        }

        let mut can_answer = stoppers;
        loop {
            let before = can_answer.len();
            for edge in &self.body.edges {
                if region.contains(&edge.source)
                    && region.contains(&edge.destination)
                    && can_answer.contains(&edge.destination)
                {
                    can_answer.insert(edge.source.clone());
                }
            }
            if can_answer.len() == before {
                break;
            }
        }
        region.is_subset(&can_answer)
    }

    fn node_for(&self, node_id: &ExecutionNodeId) -> Option<&ExecutionPlanNode> {
        self.body
            .nodes
            .iter()
            .find(|node| node.local_node_id == *node_id)
    }
}

/// Compute the byte identity of one exact stored execution plan.
pub fn execution_bundle_hash(exact_bytes: &[u8]) -> String {
    digest(exact_bytes)
}

/// Compute the semantic callable-input hash over RFC 8785 guard bytes.
pub fn entry_input_schema_hash(guard: &Value) -> String {
    wamn_flow::canonical_json_sha256(guard)
}

/// Verify exact bytes before parsing and semantically validating their plan.
pub fn read_execution_plan(
    expected_execution_bundle_hash: &str,
    exact_bytes: &[u8],
) -> Result<ExecutionPlanV2, CatalogIdentityError> {
    validate_digest(expected_execution_bundle_hash, "execution-bundle-hash")?;
    if execution_bundle_hash(exact_bytes) != expected_execution_bundle_hash {
        return Err(CatalogIdentityError::ExecutionBundleHashMismatch);
    }
    let plan = serde_json::from_slice::<ExecutionPlanV2>(exact_bytes).map_err(|error| {
        CatalogIdentityError::InvalidDefinition {
            message: format!("execution plan JSON is invalid: {error}"),
        }
    })?;
    plan.validate()?;
    Ok(plan)
}

fn deserialize_required_option<'de, D>(
    deserializer: D,
) -> Result<Option<CallableContract>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<CallableContract>::deserialize(deserializer)
}

fn validate_entry_schema(schema: &Value) -> Result<(), CatalogIdentityError> {
    if let Some(declared) = schema.get("$schema")
        && declared.as_str() != Some(JSON_SCHEMA_2020_12)
    {
        return invalid("entry input-schema guard must use JSON Schema draft 2020-12");
    }
    if has_remote_ref(schema) {
        return invalid("entry input-schema guard may only use document-local references");
    }

    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(ENTRY_SCHEMA_URI, schema.clone())
        .map_err(|error| CatalogIdentityError::InvalidDefinition {
            message: format!("entry input-schema guard is invalid: {error}"),
        })?;
    let mut schemas = Schemas::new();
    compiler
        .compile(ENTRY_SCHEMA_URI, &mut schemas)
        .map_err(|error| CatalogIdentityError::InvalidDefinition {
            message: format!("entry input-schema guard is invalid: {error}"),
        })?;
    Ok(())
}

fn has_remote_ref(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(has_remote_ref),
        Value::Object(values) => values.iter().any(|(key, value)| {
            (key == "$ref"
                && value
                    .as_str()
                    .is_none_or(|reference| !reference.starts_with('#')))
                || has_remote_ref(value)
        }),
        _ => false,
    }
}

fn reachable_from(
    start: &ExecutionNodeId,
    nodes: &[ExecutionPlanNode],
    edges: &[ExecutionPlanEdge],
    stop_at_release: bool,
) -> BTreeSet<ExecutionNodeId> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![start.clone()];
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id.clone()) {
            continue;
        }
        if stop_at_release
            && nodes.iter().any(|node| {
                node.local_node_id == node_id
                    && matches!(node.node_type.as_str(), "respond" | "fail")
            })
        {
            continue;
        }
        stack.extend(
            edges
                .iter()
                .filter(|edge| edge.source == node_id)
                .map(|edge| edge.destination.clone()),
        );
    }
    seen
}

fn valid_flow_slug(value: &str) -> bool {
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = value.as_bytes();
    bytes
        .iter()
        .all(|byte| alphanumeric(*byte) || *byte == b'-')
        && bytes.first().copied().is_some_and(alphanumeric)
        && bytes.last().copied().is_some_and(alphanumeric)
}

fn invalid<T>(message: impl Into<String>) -> Result<T, CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn node_id(value: &str) -> ExecutionNodeId {
        ExecutionNodeId::new(value).unwrap()
    }

    fn runtime_revision() -> ExecutionRuntimeRevision {
        ExecutionRuntimeRevision {
            flowrunner_component_digest: DIGEST_B.into(),
            effect_provider_revision: DIGEST_A.into(),
            host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
        }
    }

    fn node(id: &str, node_type: &str, config: Value) -> ExecutionPlanNode {
        ExecutionPlanNode {
            local_node_id: node_id(id),
            source_node_id: id.into(),
            node_type: node_type.into(),
            config,
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        }
    }

    fn source_map(nodes: &[ExecutionPlanNode]) -> Vec<ExecutionSourceMapEntry> {
        nodes
            .iter()
            .map(|node| ExecutionSourceMapEntry {
                local_node_id: node.local_node_id.clone(),
                source_node_id: node.source_node_id.clone(),
            })
            .collect()
    }

    fn event_plan() -> ExecutionPlanV2 {
        let nodes = vec![node("entry", "event", serde_json::json!({}))];
        ExecutionPlanV2::new(
            runtime_revision(),
            DIGEST_A,
            ExecutionPlanBody {
                entry_instruction: node_id("entry"),
                source_map: source_map(&nodes),
                nodes,
                edges: Vec::new(),
                root_terminal_behavior: RootTerminalBehavior::FrontierExhaustion,
                entry_input_schema_guard: Value::Bool(true),
                callable_contract: None,
            },
        )
        .unwrap()
    }

    fn request_plan() -> ExecutionPlanV2 {
        let guard = serde_json::json!({
            "$schema": JSON_SCHEMA_2020_12,
            "type": "object",
            "properties": {"name": {"type": "string"}}
        });
        let nodes = vec![
            node(
                "request",
                "request",
                serde_json::json!({"input-schema": guard}),
            ),
            node("respond-z", "respond", serde_json::json!({"status": 200})),
            node("respond-a", "respond", serde_json::json!({"status": 201})),
        ];
        let edges = vec![
            ExecutionPlanEdge {
                source: node_id("request"),
                source_port: "main".into(),
                destination: node_id("respond-z"),
                destination_port: None,
                fan_out_ordinal: 0,
            },
            ExecutionPlanEdge {
                source: node_id("request"),
                source_port: "main".into(),
                destination: node_id("respond-a"),
                destination_port: None,
                fan_out_ordinal: 1,
            },
        ];
        let entry_input_schema_guard = nodes[0].config["input-schema"].clone();
        let callable_contract = Some(CallableContract {
            version: CALLABLE_CONTRACT_VERSION.into(),
            input_schema_hash: entry_input_schema_hash(&entry_input_schema_guard),
            return_contract: CallableReturnContract::UntypedJsonBody,
            effect_ceiling: CallableEffectCeiling::Effectful,
        });
        ExecutionPlanV2::new(
            runtime_revision(),
            DIGEST_A,
            ExecutionPlanBody {
                entry_instruction: node_id("request"),
                source_map: source_map(&nodes),
                nodes,
                edges,
                root_terminal_behavior: RootTerminalBehavior::Respond {
                    responders: vec![node_id("respond-z"), node_id("respond-a")],
                },
                entry_input_schema_guard,
                callable_contract,
            },
        )
        .unwrap()
    }

    fn exact_bytes(plan: &ExecutionPlanV2) -> Vec<u8> {
        serde_json::to_vec(plan).unwrap()
    }

    #[test]
    fn own_flow_wire_has_exact_required_members_and_no_removed_shape() {
        let encoded = serde_json::to_value(event_plan()).unwrap();
        let header = encoded["header"].as_object().unwrap();
        assert_eq!(
            header.keys().cloned().collect::<BTreeSet<_>>(),
            [
                "format-version",
                "plan-compiler-revision",
                "root-artifact-hash",
                "runtime-revision",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        assert_eq!(header["format-version"], "0.1");
        assert_eq!(header["plan-compiler-revision"], "0.1");
        assert_eq!(
            header["runtime-revision"]["host-effect-contract-version"],
            "0.1"
        );
        let body = encoded["body"].as_object().unwrap();
        assert_eq!(
            body.keys().cloned().collect::<BTreeSet<_>>(),
            [
                "callable-contract",
                "edges",
                "entry-input-schema-guard",
                "entry-instruction",
                "nodes",
                "root-terminal-behavior",
                "source-map",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        assert!(encoded["body"]["entry-instruction"].is_string());
        assert!(encoded["body"]["callable-contract"].is_null());
        let node = encoded["body"]["nodes"][0].as_object().unwrap();
        assert!(node.contains_key("local-node-id"));
        assert!(!node.contains_key("path"));
        assert!(!node.contains_key("source-artifact-hash"));

        let request = serde_json::to_value(request_plan()).unwrap();
        assert_eq!(
            request["body"]["root-terminal-behavior"],
            serde_json::json!({
                "kind": "respond",
                "responders": ["respond-z", "respond-a"]
            })
        );
        assert_eq!(
            request["body"]["callable-contract"],
            serde_json::json!({
                "version": "0.1",
                "input-schema-hash": entry_input_schema_hash(
                    &request_plan().body.entry_input_schema_guard
                ),
                "return-contract": "untyped-json-body",
                "effect-ceiling": "effectful"
            })
        );
    }

    #[test]
    fn call_flow_node_has_exact_site_flow_id_contract() {
        let mut plan = event_plan();
        assert!(!plan.requires_idempotency_key());
        plan.body.nodes.push(ExecutionPlanNode {
            local_node_id: node_id("call-site"),
            source_node_id: "call-site".into(),
            node_type: "call-flow".into(),
            config: serde_json::json!({"site": "call-site", "flow-id": "callee-flow"}),
            effect_policy: ExecutionEffectPolicy::Effectful,
            source_connection_requirement: None,
        });
        plan.body.edges.push(ExecutionPlanEdge {
            source: node_id("entry"),
            source_port: "main".into(),
            destination: node_id("call-site"),
            destination_port: None,
            fan_out_ordinal: 0,
        });
        plan.body.source_map.push(ExecutionSourceMapEntry {
            local_node_id: node_id("call-site"),
            source_node_id: "call-site".into(),
        });
        assert!(plan.validate().is_ok());
        assert!(plan.requires_idempotency_key());

        let mut wrong_site = plan.clone();
        wrong_site.body.nodes[1].config["site"] = Value::String("entry".into());
        assert!(wrong_site.validate().is_err());

        let mut pure = plan.clone();
        pure.body.nodes[1].effect_policy = ExecutionEffectPolicy::Pure;
        assert!(pure.validate().is_err());

        let mut extra = plan;
        extra.body.nodes[1].config["callee-hash"] = Value::String(DIGEST_B.into());
        assert!(extra.validate().is_err());
    }

    #[test]
    fn idempotency_key_predicate_is_effectful_or_call_flow() {
        let mut plan = event_plan();
        assert!(!plan.requires_idempotency_key());

        plan.body.nodes[0].effect_policy = ExecutionEffectPolicy::Effectful;
        assert!(plan.requires_idempotency_key());
    }

    #[test]
    fn entry_guard_and_callable_contract_are_consistent() {
        let plan = request_plan();
        assert_eq!(
            plan.body
                .callable_contract
                .as_ref()
                .unwrap()
                .input_schema_hash,
            entry_input_schema_hash(&plan.body.entry_input_schema_guard)
        );

        let mut wrong_hash = plan.clone();
        wrong_hash
            .body
            .callable_contract
            .as_mut()
            .unwrap()
            .input_schema_hash = DIGEST_B.into();
        assert!(wrong_hash.validate().is_err());

        let mut missing = plan.clone();
        missing.body.callable_contract = None;
        assert!(missing.validate().is_err());

        let mut noncallable = plan;
        noncallable.body.edges.push(ExecutionPlanEdge {
            source: node_id("respond-a"),
            source_port: "main".into(),
            destination: node_id("respond-z"),
            destination_port: None,
            fan_out_ordinal: 0,
        });
        noncallable.body.callable_contract = None;
        assert!(noncallable.validate().is_ok());
    }

    #[test]
    fn callable_contract_allows_failure_paths_and_answerable_cycles() {
        let mut failure_path = request_plan();
        failure_path
            .body
            .nodes
            .push(node("fail", "fail", serde_json::json!({})));
        failure_path.body.edges.push(ExecutionPlanEdge {
            source: node_id("request"),
            source_port: "main".into(),
            destination: node_id("fail"),
            destination_port: None,
            fan_out_ordinal: 2,
        });
        failure_path.body.source_map = source_map(&failure_path.body.nodes);
        assert!(failure_path.validate().is_ok());

        let mut answerable_cycle = request_plan();
        answerable_cycle.body.nodes.extend([
            node("cycle-a", "map", serde_json::json!({})),
            node("cycle-b", "map", serde_json::json!({})),
        ]);
        answerable_cycle.body.edges.extend([
            ExecutionPlanEdge {
                source: node_id("request"),
                source_port: "main".into(),
                destination: node_id("cycle-a"),
                destination_port: None,
                fan_out_ordinal: 2,
            },
            ExecutionPlanEdge {
                source: node_id("cycle-a"),
                source_port: "main".into(),
                destination: node_id("cycle-b"),
                destination_port: None,
                fan_out_ordinal: 0,
            },
            ExecutionPlanEdge {
                source: node_id("cycle-b"),
                source_port: "main".into(),
                destination: node_id("cycle-a"),
                destination_port: None,
                fan_out_ordinal: 0,
            },
            ExecutionPlanEdge {
                source: node_id("cycle-b"),
                source_port: "main".into(),
                destination: node_id("respond-z"),
                destination_port: None,
                fan_out_ordinal: 1,
            },
        ]);
        answerable_cycle.body.source_map = source_map(&answerable_cycle.body.nodes);
        assert!(answerable_cycle.validate().is_ok());
        assert!(
            answerable_cycle
                .derived_callable_contract()
                .unwrap()
                .is_some()
        );

        answerable_cycle.body.edges.pop();
        answerable_cycle.body.callable_contract = None;
        assert!(answerable_cycle.validate().is_ok());
    }

    #[test]
    fn frontier_success_is_not_callable_and_validation_has_no_depth_cap() {
        assert_eq!(event_plan().body.callable_contract, None);
        assert_eq!(event_plan().derived_callable_contract().unwrap(), None);

        let mut plan = request_plan();
        plan.body
            .nodes
            .retain(|node| matches!(node.node_type.as_str(), "request" | "respond"));
        plan.body.edges.clear();
        let chain_len = 160;
        for index in 0..chain_len {
            plan.body
                .nodes
                .push(node(&format!("step-{index}"), "map", serde_json::json!({})));
        }
        plan.body.edges.push(ExecutionPlanEdge {
            source: node_id("request"),
            source_port: "main".into(),
            destination: node_id("step-0"),
            destination_port: None,
            fan_out_ordinal: 0,
        });
        plan.body.edges.push(ExecutionPlanEdge {
            source: node_id("request"),
            source_port: "main".into(),
            destination: node_id("respond-a"),
            destination_port: None,
            fan_out_ordinal: 1,
        });
        for index in 0..chain_len - 1 {
            plan.body.edges.push(ExecutionPlanEdge {
                source: node_id(&format!("step-{index}")),
                source_port: "main".into(),
                destination: node_id(&format!("step-{}", index + 1)),
                destination_port: None,
                fan_out_ordinal: 0,
            });
        }
        plan.body.edges.push(ExecutionPlanEdge {
            source: node_id(&format!("step-{}", chain_len - 1)),
            source_port: "main".into(),
            destination: node_id("respond-z"),
            destination_port: None,
            fan_out_ordinal: 0,
        });
        plan.body.source_map = source_map(&plan.body.nodes);
        plan.body.callable_contract = plan.derived_callable_contract().unwrap();

        assert!(plan.body.callable_contract.is_some());
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn source_map_bijectively_covers_local_nodes() {
        let mut missing = request_plan();
        missing.body.source_map.pop();
        assert!(missing.validate().is_err());

        let mut duplicate = request_plan();
        duplicate.body.source_map[1].local_node_id = node_id("request");
        assert!(duplicate.validate().is_err());

        let mut mismatch = request_plan();
        mismatch.body.source_map[0].source_node_id = "other".into();
        assert!(mismatch.validate().is_err());
    }

    #[test]
    fn retired_cron_entry_is_rejected() {
        let mut plan = event_plan();
        plan.body.nodes[0].node_type = "cron".into();
        assert!(plan.validate().is_err());
    }

    #[test]
    fn exact_byte_reader_hashes_before_parse() {
        let malformed = b"this is not JSON";
        let mismatched = DIGEST_A;
        assert_eq!(
            read_execution_plan(mismatched, malformed),
            Err(CatalogIdentityError::ExecutionBundleHashMismatch)
        );

        let matching = execution_bundle_hash(malformed);
        assert!(matches!(
            read_execution_plan(&matching, malformed),
            Err(CatalogIdentityError::InvalidDefinition { .. })
        ));
    }

    #[test]
    fn exact_byte_reader_accepts_matching_hash_noncanonical_json() {
        let plan = request_plan();
        let bytes = serde_json::to_vec_pretty(&plan).unwrap();
        let hash = execution_bundle_hash(&bytes);
        assert_eq!(read_execution_plan(&hash, &bytes).unwrap(), plan);
    }

    #[test]
    fn exact_byte_reader_rejects_semantic_invalid_after_hash_success() {
        let mut value = serde_json::to_value(event_plan()).unwrap();
        value["header"]["format-version"] = Value::String("0.2".into());
        let bytes = serde_json::to_vec(&value).unwrap();
        let hash = execution_bundle_hash(&bytes);
        assert!(matches!(
            read_execution_plan(&hash, &bytes),
            Err(CatalogIdentityError::InvalidDefinition { .. })
        ));
    }

    #[test]
    fn exact_byte_reader_requires_nullable_contract_field_and_rejects_removed_shape() {
        let mut missing = serde_json::to_value(event_plan()).unwrap();
        missing["body"]
            .as_object_mut()
            .unwrap()
            .remove("callable-contract");
        let bytes = serde_json::to_vec(&missing).unwrap();
        assert!(read_execution_plan(&execution_bundle_hash(&bytes), &bytes).is_err());

        let mut obsolete = serde_json::to_value(event_plan()).unwrap();
        obsolete["body"]["synthetic-instructions"] = Value::Array(Vec::new());
        let bytes = serde_json::to_vec(&obsolete).unwrap();
        assert!(read_execution_plan(&execution_bundle_hash(&bytes), &bytes).is_err());
    }

    #[test]
    fn exact_byte_hash_changes_with_semantic_array_order() {
        let plan = request_plan();
        let original = exact_bytes(&plan);
        let mut reordered = plan;
        let RootTerminalBehavior::Respond { responders } =
            &mut reordered.body.root_terminal_behavior
        else {
            panic!("request fixture has responders");
        };
        responders.reverse();
        let changed = exact_bytes(&reordered);
        assert_ne!(
            execution_bundle_hash(&original),
            execution_bundle_hash(&changed)
        );
        assert!(read_execution_plan(&execution_bundle_hash(&changed), &changed).is_ok());
    }

    #[test]
    fn remote_entry_schema_references_are_refused() {
        let mut plan = request_plan();
        plan.body.entry_input_schema_guard =
            serde_json::json!({"$ref": "https://example.test/schema"});
        plan.body.nodes[0].config["input-schema"] = plan.body.entry_input_schema_guard.clone();
        plan.body
            .callable_contract
            .as_mut()
            .unwrap()
            .input_schema_hash = entry_input_schema_hash(&plan.body.entry_input_schema_guard);
        assert!(plan.validate().is_err());
    }
}
