//! Declared response evidence retained for one in-memory delivery.

use anyhow::Context as _;
use boon::{Compiler, Draft, SchemaIndex, Schemas};
use serde_json::Value;
use wamn_catalog::WiringResponse;
use wamn_execution_contract::EffectOutcome;
use wamn_router::{Failure, FailureKind, NodeOutcome, Verdict, WalkStatus};
use wamn_runtime::plugins::EffectEvidence;
use wamn_runtime::plugins::wamn_postgres::ResolvedActiveWiring;

struct PreparedSchema {
    source: Value,
    schemas: Schemas,
    index: SchemaIndex,
}

impl std::fmt::Debug for PreparedSchema {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedSchema")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl PreparedSchema {
    fn new(source: Value) -> anyhow::Result<Self> {
        let mut compiler = Compiler::new();
        compiler.set_default_draft(Draft::V2020_12);
        compiler
            .add_resource("https://wamn.invalid/served-response", source.clone())
            .map_err(|error| anyhow::anyhow!("invalid declared response schema: {error}"))?;
        let mut schemas = Schemas::new();
        let index = compiler
            .compile("https://wamn.invalid/served-response", &mut schemas)
            .map_err(|error| anyhow::anyhow!("compile declared response schema: {error}"))?;
        Ok(Self {
            source,
            schemas,
            index,
        })
    }

    fn matches(&self, value: &Value) -> bool {
        self.schemas.validate(value, self.index).is_ok()
    }
}

impl PartialEq for PreparedSchema {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}
impl Eq for PreparedSchema {}

/// Compiled once when the exact wiring enters the existing resolution cache.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PreparedResponse {
    declaration: WiringResponse,
    normal: PreparedSchema,
    committed: Option<PreparedSchema>,
}

impl PreparedResponse {
    pub(crate) fn from_resolved(resolved: &ResolvedActiveWiring) -> anyhow::Result<Option<Self>> {
        let Some(declaration) = &resolved.response else {
            return Ok(None);
        };
        let committed = declaration
            .committed_result
            .as_ref()
            .map(|node_id| {
                let node = resolved
                    .wiring
                    .node(node_id)
                    .context("declared committed node is absent")?;
                let operation = resolved
                    .component_by_digest(&node.component)
                    .and_then(|component| component.operation(&node.operation))
                    .context("declared committed operation is absent")?;
                let schema = operation
                    .committed_result_schema
                    .as_ref()
                    .context("declared operation has no committed result contract")?;
                PreparedSchema::new(schema.schema.clone())
            })
            .transpose()?;
        Ok(Some(Self {
            normal: PreparedSchema::new(declaration.schema.clone())?,
            declaration: declaration.clone(),
            committed,
        }))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PartialEvidence {
    pub(crate) committed_result: Value,
    pub(crate) effect_outcome: Option<EffectOutcome>,
}

/// An interrupted invocation retains only the selected committed result.
#[derive(Debug)]
pub(crate) struct InterruptedResponse {
    pub(crate) evidence: PartialEvidence,
    pub(crate) source: anyhow::Error,
}

impl std::fmt::Display for InterruptedResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "delivery failed after a declared committed result: {}",
            self.source
        )
    }
}

impl std::error::Error for InterruptedResponse {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// One selected result, never a history of node outputs or effect attempts.
pub(crate) struct ResponseState<'a> {
    contract: Option<&'a PreparedResponse>,
    selected_seen: bool,
    ambiguous: bool,
    committed_result: Option<Value>,
    failed_effect: Option<(String, EffectOutcome)>,
    last_cancelled: bool,
}

impl<'a> ResponseState<'a> {
    pub(crate) fn new(contract: Option<&'a PreparedResponse>, caller_attached: bool) -> Self {
        Self {
            contract: contract.filter(|_| caller_attached),
            selected_seen: false,
            ambiguous: false,
            committed_result: None,
            failed_effect: None,
            last_cancelled: false,
        }
    }

    pub(crate) fn effect_evidence(&self) -> Option<EffectEvidence> {
        self.committed_result
            .as_ref()
            .filter(|_| !self.ambiguous)
            .map(|_| EffectEvidence::new())
    }

    pub(crate) fn observe(
        &mut self,
        node: &str,
        outcome: &NodeOutcome,
        effects: Option<&EffectEvidence>,
    ) -> anyhow::Result<()> {
        self.failed_effect = None;
        self.last_cancelled = matches!(outcome, NodeOutcome::Cancelled);
        let Some(contract) = self.contract else {
            return Ok(());
        };
        match outcome {
            NodeOutcome::Success { payload, .. } => {
                if contract.declaration.committed_result.as_deref() == Some(node) {
                    // A second successful visit cannot be represented by one result.
                    self.ambiguous |= self.selected_seen;
                    self.selected_seen = true;
                    self.committed_result = if !self.ambiguous
                        && contract
                            .committed
                            .as_ref()
                            .is_some_and(|schema| schema.matches(payload))
                    {
                        Some(payload.clone())
                    } else {
                        None
                    };
                }
                if contract.declaration.node == node {
                    anyhow::ensure!(
                        contract.normal.matches(payload),
                        "terminal response violates its declared schema"
                    );
                }
            }
            NodeOutcome::Error(_) | NodeOutcome::Cancelled => {
                self.failed_effect = effects
                    .and_then(EffectEvidence::outcome)
                    .map(|outcome| (node.to_owned(), outcome));
            }
        }
        Ok(())
    }

    pub(crate) fn evidence(
        &self,
        status: WalkStatus,
        failure: Option<&Failure>,
        verdict: Option<&Verdict>,
    ) -> Option<PartialEvidence> {
        if verdict.is_some()
            || self.ambiguous
            || !matches!(status, WalkStatus::Failed | WalkStatus::Cancelled)
        {
            return None;
        }
        Some(PartialEvidence {
            committed_result: self.committed_result.clone()?,
            effect_outcome: self
                .failed_effect
                .as_ref()
                .filter(|(node, _)| match (status, failure) {
                    (WalkStatus::Cancelled, None) => self.last_cancelled,
                    (WalkStatus::Failed, Some(failure)) => {
                        !self.last_cancelled
                            && failure.node == *node
                            && matches!(
                                failure.kind,
                                FailureKind::Terminal
                                    | FailureKind::RetryExhausted
                                    | FailureKind::InvalidInput
                            )
                    }
                    _ => false,
                })
                .map(|(_, outcome)| *outcome),
        })
    }

    pub(crate) fn interrupted(
        &self,
        source: anyhow::Error,
        effects: Option<&EffectEvidence>,
    ) -> anyhow::Error {
        match self.evidence(WalkStatus::Failed, None, None) {
            Some(mut evidence) => {
                evidence.effect_outcome = effects.and_then(EffectEvidence::outcome);
                InterruptedResponse { evidence, source }.into()
            }
            None => source,
        }
    }
}

#[cfg(test)]
#[path = "router_response_tests.rs"]
mod tests;
