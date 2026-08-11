//! Canonical executable-plan identity for validated flow closures.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_node_manifest::ConnectionTypeDescriptor;

use crate::{
    CatalogIdentityError, ExecutionNodePath, canonical_serialized, digest, validate_digest,
};

/// The only execution-plan format shipped by the MVP.
pub const EXECUTION_PLAN_FORMAT_VERSION: &str = "0.1";
/// The only host-effect contract understood by execution plan format 0.1.
pub const HOST_EFFECT_CONTRACT_VERSION: &str = "0.1";

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

/// One executable node in the fully expanded closure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanNode {
    pub path: ExecutionNodePath,
    pub source_artifact_hash: String,
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
    pub source: ExecutionNodePath,
    pub source_port: String,
    pub destination: ExecutionNodePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_port: Option<String>,
    pub fan_out_ordinal: u32,
}

/// How successful completion of the root graph reaches its caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum RootTerminalBehavior {
    Respond { paths: Vec<ExecutionNodePath> },
    FrontierExhaustion,
}

/// The exact continuation taken when an expanded callee responds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReturnContinuation {
    pub destination: ExecutionNodePath,
    pub destination_port: String,
}

/// Synthetic instructions introduced only by call expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum SyntheticInstruction {
    CallEnter {
        call_path: ExecutionNodePath,
        callee_entry: ExecutionNodePath,
        input_guard: ExecutionNodePath,
    },
    CallReturn {
        call_path: ExecutionNodePath,
        callee_respond: ExecutionNodePath,
        continuation: ReturnContinuation,
    },
}

/// One compiled runtime input-schema guard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RuntimeInputSchemaGuard {
    pub path: ExecutionNodePath,
    pub schema: Value,
}

/// Metadata delimiting one expanded call frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CallFrameMetadata {
    pub call_path: ExecutionNodePath,
    pub callee_artifact_hash: String,
    pub return_continuation: ReturnContinuation,
}

/// Source coordinates for one expanded node path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionSourceMapEntry {
    pub path: ExecutionNodePath,
    pub source_artifact_hash: String,
    pub source_node_id: String,
}

/// The complete executable body of a validated closure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanBody {
    pub entry_instruction: ExecutionNodePath,
    pub nodes: Vec<ExecutionPlanNode>,
    pub edges: Vec<ExecutionPlanEdge>,
    pub root_terminal_behavior: RootTerminalBehavior,
    pub synthetic_instructions: Vec<SyntheticInstruction>,
    pub input_schema_guards: Vec<RuntimeInputSchemaGuard>,
    pub call_frames: Vec<CallFrameMetadata>,
    pub source_map: Vec<ExecutionSourceMapEntry>,
}

impl ExecutionPlanBody {
    fn normalize(&mut self) {
        if let RootTerminalBehavior::Respond { paths } = &mut self.root_terminal_behavior {
            paths.sort();
        }
        for node in &mut self.nodes {
            if let Some(requirement) = &mut node.source_connection_requirement {
                requirement
                    .descriptor
                    .field_ownership
                    .sort_by_key(|ownership| ownership.field);
            }
        }
        self.nodes.sort_by(|left, right| left.path.cmp(&right.path));
        self.edges.sort_by(|left, right| {
            (
                &left.source,
                &left.source_port,
                left.fan_out_ordinal,
                &left.destination,
                &left.destination_port,
            )
                .cmp(&(
                    &right.source,
                    &right.source_port,
                    right.fan_out_ordinal,
                    &right.destination,
                    &right.destination_port,
                ))
        });
        self.synthetic_instructions.sort_by(|left, right| {
            synthetic_instruction_key(left).cmp(&synthetic_instruction_key(right))
        });
        self.input_schema_guards
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.call_frames
            .sort_by(|left, right| left.call_path.cmp(&right.call_path));
        self.source_map
            .sort_by(|left, right| left.path.cmp(&right.path));
    }
}

/// The sole canonical executable representation stored for a validated flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutionPlanV2 {
    pub header: ExecutionPlanHeader,
    pub body: ExecutionPlanBody,
}

impl ExecutionPlanV2 {
    pub(crate) fn into_canonical(mut self) -> Result<Self, CatalogIdentityError> {
        self.body.normalize();
        self.validate()?;
        Ok(self)
    }

    /// Validate and normalize one complete plan.
    pub fn new(
        runtime_revision: ExecutionRuntimeRevision,
        root_artifact_hash: impl Into<String>,
        body: ExecutionPlanBody,
    ) -> Result<Self, CatalogIdentityError> {
        let plan = Self {
            header: ExecutionPlanHeader {
                format_version: EXECUTION_PLAN_FORMAT_VERSION.to_string(),
                runtime_revision,
                root_artifact_hash: root_artifact_hash.into(),
            },
            body,
        };
        plan.into_canonical()
    }

    /// Parse bytes that must already be the exact canonical plan representation.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, CatalogIdentityError> {
        let mut plan: Self = serde_json::from_slice(bytes).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("execution plan JSON is invalid: {error}"),
            }
        })?;
        let original = plan.clone();
        plan.body.normalize();
        plan.validate()?;
        if plan != original || plan.canonical_bytes()? != bytes {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        Ok(plan)
    }

    /// RFC 8785 canonical UTF-8 JSON bytes used as the bundle hash preimage.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CatalogIdentityError> {
        let mut canonical = self.clone();
        canonical.body.normalize();
        canonical.validate()?;
        Ok(canonical_serialized(&canonical))
    }

    /// Content address of [`Self::canonical_bytes`].
    pub fn execution_bundle_hash(&self) -> Result<String, CatalogIdentityError> {
        Ok(digest(&self.canonical_bytes()?))
    }

    fn validate(&self) -> Result<(), CatalogIdentityError> {
        if self.header.format_version != EXECUTION_PLAN_FORMAT_VERSION {
            return invalid("execution plan format-version must be textual 0.1");
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
        if self.body.nodes.is_empty() {
            return invalid("execution plan must contain at least one node");
        }

        let paths = self
            .body
            .nodes
            .iter()
            .map(|node| &node.path)
            .collect::<BTreeSet<_>>();
        if paths.len() != self.body.nodes.len() {
            return invalid("execution plan node paths must be unique");
        }
        if !paths.contains(&self.body.entry_instruction) {
            return invalid("execution plan entry instruction must name a plan node");
        }
        let node_for =
            |path: &ExecutionNodePath| self.body.nodes.iter().find(|node| node.path == *path);
        let entry = node_for(&self.body.entry_instruction)
            .expect("execution plan entry membership checked");
        if entry.source_artifact_hash != self.header.root_artifact_hash {
            return invalid("execution plan entry must belong to the root artifact");
        }
        for node in &self.body.nodes {
            validate_digest(&node.source_artifact_hash, "source-artifact-hash")?;
            if node.source_node_id.is_empty() || node.node_type.is_empty() {
                return invalid("execution plan source node id and type must not be empty");
            }
            if let Some(requirement) = &node.source_connection_requirement {
                if requirement.name.is_empty()
                    || !requirement.name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
                {
                    return invalid("execution plan connection requirement name must be a slug");
                }
                if requirement.descriptor != ConnectionTypeDescriptor::http_v1() {
                    return invalid(
                        "execution plan connection descriptor must be canonical HTTP v1",
                    );
                }
            }
        }
        let mut fan_out = BTreeSet::new();
        for edge in &self.body.edges {
            if !paths.contains(&edge.source) || !paths.contains(&edge.destination) {
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
        for source in &self.body.source_map {
            validate_digest(&source.source_artifact_hash, "source-artifact-hash")?;
            if !paths.contains(&source.path) || source.source_node_id.is_empty() {
                return invalid("execution source-map entries must name existing nodes");
            }
            let node = self
                .body
                .nodes
                .iter()
                .find(|node| node.path == source.path)
                .expect("source-map path membership checked");
            if source.source_artifact_hash != node.source_artifact_hash
                || source.source_node_id != node.source_node_id
            {
                return invalid("execution source-map entry must match its plan node source");
            }
        }
        if self
            .body
            .source_map
            .windows(2)
            .any(|pair| pair[0].path == pair[1].path)
        {
            return invalid("execution source-map paths must be unique");
        }
        if self.body.source_map.len() != self.body.nodes.len() {
            return invalid("execution source-map must cover every plan node exactly once");
        }
        match (&entry.node_type[..], &self.body.root_terminal_behavior) {
            ("request", RootTerminalBehavior::FrontierExhaustion) => {
                return invalid("request-entry plans require a root responder");
            }
            ("request", RootTerminalBehavior::Respond { paths: responders }) => {
                if responders.is_empty()
                    || responders.iter().collect::<BTreeSet<_>>().len() != responders.len()
                {
                    return invalid("request root responders must be nonempty and unique");
                }
                for path in responders {
                    let Some(responder) = node_for(path) else {
                        return invalid("execution plan root terminal must name existing nodes");
                    };
                    if responder.node_type != "respond"
                        || responder.source_artifact_hash != self.header.root_artifact_hash
                    {
                        return invalid(
                            "root responders must be respond nodes from the root artifact",
                        );
                    }
                }
                let complete_root_responders = self
                    .body
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.node_type == "respond"
                            && node.source_artifact_hash == self.header.root_artifact_hash
                    })
                    .map(|node| &node.path);
                if responders.iter().ne(complete_root_responders) {
                    return invalid(
                        "request root responders must name every respond node from the root artifact",
                    );
                }
            }
            (_, RootTerminalBehavior::Respond { .. }) => {
                return invalid("only request-entry plans may name root responders");
            }
            (_, RootTerminalBehavior::FrontierExhaustion) => {}
        }
        if self
            .body
            .synthetic_instructions
            .windows(2)
            .any(|pair| synthetic_instruction_key(&pair[0]) == synthetic_instruction_key(&pair[1]))
        {
            return invalid("synthetic instruction keys must be unique");
        }
        for instruction in &self.body.synthetic_instructions {
            let referenced = match instruction {
                SyntheticInstruction::CallEnter {
                    call_path,
                    callee_entry,
                    input_guard,
                } => [call_path, callee_entry, input_guard],
                SyntheticInstruction::CallReturn {
                    call_path,
                    callee_respond,
                    continuation,
                } => [call_path, callee_respond, &continuation.destination],
            };
            if referenced.into_iter().any(|path| !paths.contains(path)) {
                return invalid("synthetic instructions must name existing plan nodes");
            }
        }
        if self
            .body
            .input_schema_guards
            .windows(2)
            .any(|pair| pair[0].path == pair[1].path)
        {
            return invalid("runtime input-schema guard paths must be unique");
        }
        if self
            .body
            .input_schema_guards
            .iter()
            .any(|guard| !paths.contains(&guard.path))
        {
            return invalid("runtime input-schema guards must name existing plan nodes");
        }
        if self
            .body
            .call_frames
            .windows(2)
            .any(|pair| pair[0].call_path == pair[1].call_path)
        {
            return invalid("call-frame paths must be unique");
        }
        for frame in &self.body.call_frames {
            validate_digest(&frame.callee_artifact_hash, "callee-artifact-hash")?;
            if !paths.contains(&frame.call_path)
                || !paths.contains(&frame.return_continuation.destination)
                || frame.return_continuation.destination_port.is_empty()
            {
                return invalid("call-frame paths and continuations must name existing plan nodes");
            }
            if node_for(&frame.call_path).is_none_or(|node| node.node_type != "call-flow") {
                return invalid("call-frame call path must name a call-flow plan node");
            }
            let enters = self
                .body
                .synthetic_instructions
                .iter()
                .filter(|instruction| {
                    matches!(instruction, SyntheticInstruction::CallEnter { call_path, .. } if call_path == &frame.call_path)
                })
                .count();
            let returns = self
                .body
                .synthetic_instructions
                .iter()
                .filter(|instruction| {
                    matches!(instruction, SyntheticInstruction::CallReturn { call_path, .. } if call_path == &frame.call_path)
                })
                .count();
            if enters != 1 || returns == 0 {
                return invalid(
                    "every call frame requires exactly one enter and at least one return",
                );
            }
        }
        for instruction in &self.body.synthetic_instructions {
            match instruction {
                SyntheticInstruction::CallEnter {
                    call_path,
                    callee_entry,
                    input_guard,
                } => {
                    let Some(frame) = self
                        .body
                        .call_frames
                        .iter()
                        .find(|frame| frame.call_path == *call_path)
                    else {
                        return invalid("call-enter instruction must have call-frame metadata");
                    };
                    if !self
                        .body
                        .input_schema_guards
                        .iter()
                        .any(|guard| guard.path == *input_guard)
                        || node_for(callee_entry).is_none_or(|node| {
                            node.source_artifact_hash != frame.callee_artifact_hash
                        })
                    {
                        return invalid("call-enter guard and callee entry must match its frame");
                    }
                }
                SyntheticInstruction::CallReturn {
                    call_path,
                    callee_respond,
                    continuation,
                } => {
                    let Some(frame) = self
                        .body
                        .call_frames
                        .iter()
                        .find(|frame| frame.call_path == *call_path)
                    else {
                        return invalid("call-return instruction must have call-frame metadata");
                    };
                    if continuation.destination_port.is_empty()
                        || continuation != &frame.return_continuation
                        || node_for(callee_respond).is_none_or(|node| {
                            node.node_type != "respond"
                                || node.source_artifact_hash != frame.callee_artifact_hash
                        })
                    {
                        return invalid(
                            "call-return responder and continuation must match its frame",
                        );
                    }
                }
            }
        }
        for guard in &self.body.input_schema_guards {
            if !self.body.synthetic_instructions.iter().any(|instruction| {
                matches!(instruction, SyntheticInstruction::CallEnter { input_guard, .. } if input_guard == &guard.path)
            }) {
                return invalid("runtime input-schema guards must be referenced by call-enter");
            }
        }
        Ok(())
    }
}

fn synthetic_instruction_key(
    instruction: &SyntheticInstruction,
) -> (&ExecutionNodePath, u8, &ExecutionNodePath) {
    match instruction {
        SyntheticInstruction::CallEnter {
            call_path,
            callee_entry,
            ..
        } => (call_path, 0, callee_entry),
        SyntheticInstruction::CallReturn {
            call_path,
            callee_respond,
            ..
        } => (call_path, 1, callee_respond),
    }
}

fn invalid<T>(message: &str) -> Result<T, CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn path(value: &str) -> ExecutionNodePath {
        value.parse().unwrap()
    }

    fn plan(reverse_unordered_members: bool) -> ExecutionPlanV2 {
        let node = |path_value: &str, source: &str, node_type: &str| ExecutionPlanNode {
            path: path(path_value),
            source_artifact_hash: source.into(),
            source_node_id: path_value.replace('/', "-"),
            node_type: node_type.into(),
            config: serde_json::json!({}),
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        };
        let mut descriptor = ConnectionTypeDescriptor::http_v1();
        if reverse_unordered_members {
            descriptor.field_ownership.reverse();
        }
        let mut nodes = vec![
            node("entry", DIGEST_A, "event"),
            ExecutionPlanNode {
                source_connection_requirement: Some(ExecutionConnectionRequirement {
                    name: "database".into(),
                    descriptor,
                }),
                ..node("call-a", DIGEST_A, "call-flow")
            },
            node("call-b", DIGEST_A, "call-flow"),
            node("call-a/entry", DIGEST_B, "request"),
            node("call-a/respond", DIGEST_B, "respond"),
            node("call-b/entry", DIGEST_B, "request"),
            node("call-b/respond", DIGEST_B, "respond"),
        ];
        let mut edges = vec![
            ExecutionPlanEdge {
                source: path("entry"),
                source_port: "main".into(),
                destination: path("call-a"),
                destination_port: Some("main".into()),
                fan_out_ordinal: 0,
            },
            ExecutionPlanEdge {
                source: path("entry"),
                source_port: "main".into(),
                destination: path("call-b"),
                destination_port: Some("main".into()),
                fan_out_ordinal: 1,
            },
        ];
        let continuation = ReturnContinuation {
            destination: path("entry"),
            destination_port: "main".into(),
        };
        let mut synthetic_instructions = vec![
            SyntheticInstruction::CallEnter {
                call_path: path("call-a"),
                callee_entry: path("call-a/entry"),
                input_guard: path("call-a/entry"),
            },
            SyntheticInstruction::CallReturn {
                call_path: path("call-a"),
                callee_respond: path("call-a/respond"),
                continuation: continuation.clone(),
            },
            SyntheticInstruction::CallEnter {
                call_path: path("call-b"),
                callee_entry: path("call-b/entry"),
                input_guard: path("call-b/entry"),
            },
            SyntheticInstruction::CallReturn {
                call_path: path("call-b"),
                callee_respond: path("call-b/respond"),
                continuation: continuation.clone(),
            },
        ];
        let mut input_schema_guards = vec![
            RuntimeInputSchemaGuard {
                path: path("call-a/entry"),
                schema: serde_json::json!({"type": "object"}),
            },
            RuntimeInputSchemaGuard {
                path: path("call-b/entry"),
                schema: serde_json::json!({"type": "string"}),
            },
        ];
        let mut call_frames = vec![
            CallFrameMetadata {
                call_path: path("call-a"),
                callee_artifact_hash: DIGEST_B.into(),
                return_continuation: continuation.clone(),
            },
            CallFrameMetadata {
                call_path: path("call-b"),
                callee_artifact_hash: DIGEST_B.into(),
                return_continuation: continuation,
            },
        ];
        let mut source_map = nodes
            .iter()
            .map(|node| ExecutionSourceMapEntry {
                path: node.path.clone(),
                source_artifact_hash: node.source_artifact_hash.clone(),
                source_node_id: node.source_node_id.clone(),
            })
            .collect::<Vec<_>>();
        if reverse_unordered_members {
            nodes.reverse();
            edges.reverse();
            synthetic_instructions.reverse();
            input_schema_guards.reverse();
            call_frames.reverse();
            source_map.reverse();
        }
        ExecutionPlanV2::new(
            ExecutionRuntimeRevision {
                flowrunner_component_digest: DIGEST_B.into(),
                effect_provider_revision: DIGEST_A.into(),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
            },
            DIGEST_A,
            ExecutionPlanBody {
                entry_instruction: path("entry"),
                nodes,
                edges,
                root_terminal_behavior: RootTerminalBehavior::FrontierExhaustion,
                synthetic_instructions,
                input_schema_guards,
                call_frames,
                source_map,
            },
        )
        .unwrap()
    }

    fn minimal_plan() -> ExecutionPlanV2 {
        ExecutionPlanV2::new(
            ExecutionRuntimeRevision {
                flowrunner_component_digest: DIGEST_B.into(),
                effect_provider_revision: DIGEST_A.into(),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
            },
            DIGEST_A,
            ExecutionPlanBody {
                entry_instruction: path("entry"),
                nodes: vec![ExecutionPlanNode {
                    path: path("entry"),
                    source_artifact_hash: DIGEST_A.into(),
                    source_node_id: "entry".into(),
                    node_type: "event".into(),
                    config: serde_json::json!({}),
                    effect_policy: ExecutionEffectPolicy::Pure,
                    source_connection_requirement: None,
                }],
                edges: Vec::new(),
                root_terminal_behavior: RootTerminalBehavior::FrontierExhaustion,
                synthetic_instructions: Vec::new(),
                input_schema_guards: Vec::new(),
                call_frames: Vec::new(),
                source_map: vec![ExecutionSourceMapEntry {
                    path: path("entry"),
                    source_artifact_hash: DIGEST_A.into(),
                    source_node_id: "entry".into(),
                }],
            },
        )
        .unwrap()
    }

    #[test]
    fn rfc8785_bytes_match_utf16_key_and_ecmascript_number_vector() {
        let input: Value = serde_json::from_str(
            r#"{"ordering":{"€":"Euro Sign","\r":"Carriage Return","דּ":"Hebrew Letter Dalet With Dagesh","1":"One","😀":"Emoji: Grinning Face","":"Control","ö":"Latin Small Letter O With Diaeresis"},"numbers":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001]}"#,
        )
        .unwrap();
        assert_eq!(
            input["numbers"][0].as_f64().unwrap().to_bits(),
            333_333_333.333_333_3_f64.to_bits(),
            "RFC 8785 input must parse to the same IEEE-754 value as ECMAScript",
        );
        let expected = r#"{"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"ordering":{"\r":"Carriage Return","1":"One","":"Control","ö":"Latin Small Letter O With Diaeresis","€":"Euro Sign","😀":"Emoji: Grinning Face","דּ":"Hebrew Letter Dalet With Dagesh"}}"#;
        assert_eq!(canonical_serialized(&input), expected.as_bytes());
    }

    #[test]
    fn minimal_plan_has_frozen_exact_bytes_and_hash() {
        let expected = br#"{"body":{"call-frames":[],"edges":[],"entry-instruction":["entry"],"input-schema-guards":[],"nodes":[{"config":{},"effect-policy":"pure","path":["entry"],"source-artifact-hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source-node-id":"entry","type":"event"}],"root-terminal-behavior":{"kind":"frontier-exhaustion"},"source-map":[{"path":["entry"],"source-artifact-hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source-node-id":"entry"}],"synthetic-instructions":[]},"header":{"format-version":"0.1","root-artifact-hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","runtime-revision":{"effect-provider-revision":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","flowrunner-component-digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","host-effect-contract-version":"0.1"}}}"#;
        let plan = minimal_plan();
        assert_eq!(plan.canonical_bytes().unwrap(), expected);
        assert_eq!(
            plan.execution_bundle_hash().unwrap(),
            "sha256:3724f5785c83c68640e1727695e54cc93f04a6d1eddf4c8e061c85025e985c49"
        );
    }

    #[test]
    fn request_root_responders_are_complete_sorted_unique_and_nonempty() {
        let mut source = minimal_plan();
        source.body.nodes[0].node_type = "request".into();
        for responder in ["respond-z", "respond-a"] {
            source.body.nodes.push(ExecutionPlanNode {
                path: path(responder),
                source_artifact_hash: DIGEST_A.into(),
                source_node_id: responder.into(),
                node_type: "respond".into(),
                config: serde_json::json!({}),
                effect_policy: ExecutionEffectPolicy::Pure,
                source_connection_requirement: None,
            });
            source.body.source_map.push(ExecutionSourceMapEntry {
                path: path(responder),
                source_artifact_hash: DIGEST_A.into(),
                source_node_id: responder.into(),
            });
        }
        source.body.root_terminal_behavior = RootTerminalBehavior::Respond {
            paths: vec![path("respond-z"), path("respond-a")],
        };

        let canonical = source.into_canonical().unwrap();
        let RootTerminalBehavior::Respond { paths } = &canonical.body.root_terminal_behavior else {
            panic!("request plan must retain responder terminals");
        };
        assert_eq!(paths, &vec![path("respond-a"), path("respond-z")]);

        let mut duplicate = canonical.clone();
        duplicate.body.root_terminal_behavior = RootTerminalBehavior::Respond {
            paths: vec![path("respond-a"), path("respond-a")],
        };
        assert!(duplicate.canonical_bytes().is_err());

        let mut omitted = canonical.clone();
        omitted.body.root_terminal_behavior = RootTerminalBehavior::Respond {
            paths: vec![path("respond-a")],
        };
        assert!(omitted.canonical_bytes().is_err());
        let omitted_bytes = canonical_serialized(&omitted);
        assert!(ExecutionPlanV2::from_canonical_bytes(&omitted_bytes).is_err());

        let mut empty = canonical;
        empty.body.root_terminal_behavior = RootTerminalBehavior::Respond { paths: Vec::new() };
        assert!(empty.canonical_bytes().is_err());
    }

    #[test]
    fn exact_canonical_bytes_round_trip_and_normalize_named_collections() {
        let canonical = plan(false);
        let reversed = plan(true);
        assert_eq!(
            canonical.canonical_bytes().unwrap(),
            reversed.canonical_bytes().unwrap()
        );
        assert_eq!(
            canonical.execution_bundle_hash().unwrap(),
            reversed.execution_bundle_hash().unwrap()
        );
        assert_eq!(
            ExecutionPlanV2::from_canonical_bytes(&canonical.canonical_bytes().unwrap()).unwrap(),
            canonical
        );
    }

    #[test]
    fn edge_and_return_continuation_mutations_change_the_bundle_hash() {
        let original = plan(false);
        let mut destination_mutation = original.clone();
        destination_mutation.body.edges[0].destination = path("call-b");
        let mut ordinal_mutation = original.clone();
        ordinal_mutation.body.edges[0].fan_out_ordinal = 2;
        let mut return_mutation = original.clone();
        let call_path = return_mutation
            .body
            .synthetic_instructions
            .iter_mut()
            .find_map(|instruction| match instruction {
                SyntheticInstruction::CallReturn {
                    call_path,
                    continuation,
                    ..
                } => {
                    continuation.destination_port = "error".into();
                    Some(call_path.clone())
                }
                SyntheticInstruction::CallEnter { .. } => None,
            })
            .expect("fixture contains a call return");
        return_mutation
            .body
            .call_frames
            .iter_mut()
            .find(|frame| frame.call_path == call_path)
            .expect("fixture return has a call frame")
            .return_continuation
            .destination_port = "error".into();
        assert_ne!(
            original.execution_bundle_hash().unwrap(),
            destination_mutation.execution_bundle_hash().unwrap()
        );
        assert_ne!(
            original.execution_bundle_hash().unwrap(),
            ordinal_mutation.execution_bundle_hash().unwrap()
        );
        assert_ne!(
            original.execution_bundle_hash().unwrap(),
            return_mutation.execution_bundle_hash().unwrap()
        );
    }

    #[test]
    fn every_embedded_path_and_source_mapping_is_cross_checked() {
        let original = plan(false);

        let mut missing_terminal = original.clone();
        missing_terminal.body.root_terminal_behavior = RootTerminalBehavior::Respond {
            paths: vec![path("missing")],
        };
        assert!(missing_terminal.validate().is_err());

        let mut missing_synthetic_target = original.clone();
        let continuation = missing_synthetic_target
            .body
            .synthetic_instructions
            .iter_mut()
            .find_map(|instruction| match instruction {
                SyntheticInstruction::CallReturn { continuation, .. } => Some(continuation),
                SyntheticInstruction::CallEnter { .. } => None,
            })
            .expect("fixture contains a call return");
        continuation.destination = path("missing");
        assert!(missing_synthetic_target.validate().is_err());

        let mut incomplete_source_map = original.clone();
        incomplete_source_map.body.source_map.pop();
        assert!(incomplete_source_map.validate().is_err());

        let mut mismatched_source_map = original;
        mismatched_source_map.body.source_map[0].source_node_id = "other".into();
        assert!(mismatched_source_map.validate().is_err());
    }

    #[test]
    fn semantic_array_order_is_retained_in_plan_identity() {
        let original = plan(false);
        let mut reordered = original.clone();
        reordered.body.nodes[0].config = serde_json::json!({"steps": ["first", "second"]});
        let first = reordered.execution_bundle_hash().unwrap();
        reordered.body.nodes[0].config = serde_json::json!({"steps": ["second", "first"]});
        assert_ne!(first, reordered.execution_bundle_hash().unwrap());
    }

    #[test]
    fn wrong_host_contract_version_cannot_acquire_a_bundle_hash() {
        let mut invalid = plan(false);
        invalid.header.runtime_revision.host_effect_contract_version = "0.2".into();
        assert!(invalid.canonical_bytes().is_err());
        assert!(invalid.execution_bundle_hash().is_err());
    }
}
