//! FIFO walk, with one serializable boundary after the first Response.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::bindings::wamn::workflow::node::{self, Detail, Emission, Invocation, NodeError};
use crate::bindings::wamn::workflow::run::{Completion, Failure, FailureType, Refusal};
use crate::bindings::wasi::clocks::monotonic_clock;
use crate::graph::{self, Graph, Node};
use crate::retry::RetryPolicy;

const NANOS_PER_MS: u64 = 1_000_000;

// The old router's per-delivery cap. Each retry also spends a hop.
const HOP_LIMIT: u64 = 10_000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Walk {
    frontier: VecDeque<Token>,
    hops: u64,
    visits: HashMap<String, u32>,
    last: Option<Rc<String>>,
    pub(crate) response: Option<Response>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Token {
    node: String,
    input_port: Option<String>,
    input_json: Rc<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body_json: Rc<String>,
}

pub(crate) enum Boundary {
    Response,
    Complete,
}

impl Walk {
    pub(crate) fn start(graph: &Graph, input_json: String) -> Result<Self, WalkError> {
        serde_json::from_str::<&RawValue>(&input_json)
            .map_err(|source| WalkError::invalid_json("read trigger input", source))?;
        Ok(Self {
            frontier: VecDeque::from([Token {
                node: graph.entry.clone(),
                input_port: None,
                input_json: Rc::new(input_json),
            }]),
            hops: 0,
            visits: HashMap::new(),
            last: None,
            response: None,
        })
    }

    pub(crate) fn resume(checkpoint_json: &str) -> Result<Self, WalkError> {
        let walk: Self = serde_json::from_str(checkpoint_json)
            .map_err(|source| WalkError::invalid_json("read Response checkpoint", source))?;
        if walk.response.is_none() {
            return Err(WalkError::failed(
                FailureType::InvalidInput,
                "invalid-checkpoint",
                "checkpoint has no Response",
            ));
        }
        Ok(walk)
    }

    pub(crate) fn pending(&self) -> bool {
        !self.frontier.is_empty()
    }

    pub(crate) async fn advance(
        &mut self,
        graph: &Graph,
        handle: &str,
        deadline: Option<u64>,
    ) -> Result<Boundary, WalkError> {
        while let Some(token) = self.frontier.pop_front() {
            remaining_ms(deadline)?;
            match graph.nodes.get(&token.node) {
                Some(Node::Entry(_)) => {
                    self.spend_hop(&token.node)?;
                    self.emit(graph, token.node, graph::MAIN, token.input_json);
                }
                Some(Node::Call(call)) => match self.invoke(call, &token, handle, deadline).await {
                    Ok(emission) => self.emit(
                        graph,
                        token.node,
                        emission.port.as_deref().unwrap_or(graph::MAIN),
                        Rc::new(emission.payload_json),
                    ),
                    Err(error) if error.routable() => {
                        let targets = graph.successors(&token.node, graph::ERROR);
                        if targets.is_empty() {
                            return Err(error);
                        }
                        self.count_visit(token.node);
                        let payload =
                            serde_json::json!({ "error": ErrorDetail::routed(&error.detail) });
                        self.enqueue(targets, Rc::new(payload.to_string()));
                    }
                    Err(error) => return Err(error),
                },
                Some(Node::Response(response)) => {
                    self.spend_hop(&token.node)?;
                    if self.response.is_some() {
                        return Err(WalkError::failed(
                            FailureType::Terminal,
                            "second-verdict",
                            "workflow reached a second Response",
                        )
                        .at(&token.node));
                    }
                    self.last = None;
                    self.response = Some(Response {
                        status: response.status,
                        body_json: token.input_json,
                    });
                    return Ok(Boundary::Response);
                }
                None => {
                    return Err(WalkError::failed(
                        FailureType::UnresolvableName,
                        "unresolvable-name",
                        "checkpoint node is absent from the graph",
                    )
                    .at(&token.node));
                }
            }
        }
        if graph.request && self.response.is_none() {
            return Err(WalkError::failed(
                FailureType::Terminal,
                "unreleased-caller",
                "Request workflow ended without a Response",
            )
            .at(&graph.entry));
        }
        Ok(Boundary::Complete)
    }

    fn emit(&mut self, graph: &Graph, node: String, port: &str, payload: Rc<String>) {
        let targets = graph.successors(&node, port);
        self.count_visit(node);
        if self.response.is_none() {
            self.last = Some(Rc::clone(&payload));
        }
        self.enqueue(targets, payload);
    }

    fn enqueue(&mut self, targets: &[graph::Target], payload: Rc<String>) {
        if let Some((last, earlier)) = targets.split_last() {
            for target in earlier {
                self.frontier.push_back(Token {
                    node: target.node.clone(),
                    input_port: target.input_port.clone(),
                    input_json: Rc::clone(&payload),
                });
            }
            self.frontier.push_back(Token {
                node: last.node.clone(),
                input_port: last.input_port.clone(),
                input_json: payload,
            });
        }
    }

    fn count_visit(&mut self, node: String) {
        *self.visits.entry(node).or_default() += 1;
    }

    fn spend_hop(&mut self, node: &str) -> Result<(), WalkError> {
        if self.hops >= HOP_LIMIT {
            return Err(WalkError::failed(
                FailureType::RunawayBudget,
                "hop-limit",
                format!("hop limit of {HOP_LIMIT} spent without terminating"),
            )
            .at(node));
        }
        self.hops += 1;
        Ok(())
    }

    async fn invoke(
        &mut self,
        call: &graph::Call,
        token: &Token,
        handle: &str,
        deadline: Option<u64>,
    ) -> Result<Emission, WalkError> {
        let policy = RetryPolicy::from_config(&call.params);
        let config_json = serde_json::to_string(&call.params).map_err(|source| {
            WalkError::invalid_json("encode node parameters", source).at(&token.node)
        })?;
        let node_deadline_ms = call
            .params
            .get("deadline-ms")
            .and_then(serde_json::Value::as_u64)
            .map_or(u64::MAX, |ms| ms.max(1));
        let occurrence = self.visits.get(&token.node).copied().unwrap_or(0);
        let mut attempt = 0;
        loop {
            let remaining_ms = remaining_ms(deadline)?.min(node_deadline_ms);
            self.spend_hop(&token.node)?;
            let result = node::call(Invocation {
                handle: handle.to_owned(),
                sibling: call.sibling.clone(),
                export: call.export.clone(),
                node_id: token.node.clone(),
                input_port: token.input_port.clone(),
                occurrence,
                config_json: config_json.clone(),
                input_json: token.input_json.as_ref().clone(),
                remaining_ms,
            })
            .await;
            let (detail, retry_after_ms) = match result {
                Ok(emission) => {
                    serde_json::from_str::<&RawValue>(&emission.payload_json).map_err(
                        |source| {
                            WalkError::json_error(
                                FailureType::Terminal,
                                "read node emission",
                                source,
                            )
                            .at(&token.node)
                        },
                    )?;
                    return Ok(emission);
                }
                Err(NodeError::Retryable(detail)) => (detail, None),
                Err(NodeError::RateLimited(throttle)) => (throttle.error, throttle.retry_after_ms),
                Err(NodeError::Terminal(detail)) => {
                    let kind = if detail.code.as_deref() == Some("unresolvable-name") {
                        FailureType::UnresolvableName
                    } else {
                        FailureType::Terminal
                    };
                    return Err(WalkError::node(kind, detail).at(&token.node));
                }
                Err(NodeError::InvalidInput(detail)) => {
                    return Err(WalkError::node(FailureType::InvalidInput, detail).at(&token.node));
                }
                Err(NodeError::Cancelled) => return Err(WalkError::cancelled().at(&token.node)),
            };
            if attempt + 1 >= policy.attempts {
                return Err(WalkError::node(FailureType::RetryExhausted, detail).at(&token.node));
            }
            let delay = retry_after_ms.unwrap_or_else(|| policy.backoff_ms(attempt));
            monotonic_clock::wait_for(delay.saturating_mul(NANOS_PER_MS)).await;
            attempt += 1;
        }
    }

    pub(crate) fn result_json(&mut self) -> Result<String, serde_json::Error> {
        match &self.response {
            Some(response) => response_result(response, None),
            None => Ok(self
                .last
                .take()
                .map_or_else(|| String::from("null"), Rc::unwrap_or_clone)),
        }
    }
}

fn remaining_ms(deadline: Option<u64>) -> Result<u64, WalkError> {
    let Some(deadline) = deadline else {
        return Ok(u64::MAX);
    };
    let remaining = deadline.saturating_sub(monotonic_clock::now());
    if remaining == 0 {
        return Err(WalkError::cancelled());
    }
    Ok(remaining.div_ceil(NANOS_PER_MS))
}

#[derive(Debug)]
pub(crate) struct WalkError {
    failure_type: Option<FailureType>,
    node: Option<String>,
    detail: Detail,
    source: Option<serde_json::Error>,
}

impl WalkError {
    pub(crate) fn failed(kind: FailureType, code: &str, message: impl Into<String>) -> Self {
        Self::node(
            kind,
            Detail {
                code: Some(code.into()),
                message: message.into(),
            },
        )
    }

    fn node(kind: FailureType, detail: Detail) -> Self {
        Self {
            failure_type: Some(kind),
            node: None,
            detail,
            source: None,
        }
    }

    fn cancelled() -> Self {
        Self {
            failure_type: None,
            node: None,
            detail: Detail {
                code: None,
                message: "workflow walk cancelled".into(),
            },
            source: None,
        }
    }

    pub(crate) fn invalid_json(context: &str, source: serde_json::Error) -> Self {
        Self::json_error(FailureType::InvalidInput, context, source)
    }

    fn json_error(kind: FailureType, context: &str, source: serde_json::Error) -> Self {
        Self {
            source: Some(source),
            ..Self::failed(kind, "invalid-json", context)
        }
    }

    fn at(mut self, node: &str) -> Self {
        self.node = Some(node.into());
        self
    }

    fn routable(&self) -> bool {
        self.failure_type
            .is_some_and(|kind| !matches!(kind, FailureType::RunawayBudget))
    }

    pub(crate) fn refusal(&self) -> Refusal {
        match self.failure_type {
            Some(FailureType::InvalidInput | FailureType::ForeignRevision) => {
                Refusal::InvalidInput(self.to_string())
            }
            _ => Refusal::Unavailable,
        }
    }

    pub(crate) fn completion(
        &self,
        response: Option<&Response>,
    ) -> Result<Option<Completion>, serde_json::Error> {
        let Some(failure_type) = self.failure_type else {
            // As in the router, cancellation leaves the lease for recovery.
            return Ok(None);
        };
        let error = ErrorDetail {
            code: Some(self.detail.code.as_deref().unwrap_or(match failure_type {
                FailureType::RetryExhausted => "retry-exhausted",
                FailureType::InvalidInput => "invalid-input",
                _ => "terminal",
            })),
            message: &self.detail.message,
            node: self.node.as_deref(),
        };
        let result_json = match response {
            Some(response) => response_result(response, Some(error))?,
            None => serde_json::json!({ "error": error }).to_string(),
        };
        Ok(Some(Completion::Failed(Failure {
            failure_type,
            result_json,
        })))
    }
}

impl fmt::Display for WalkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail.message)?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source as _)
    }
}

#[derive(Serialize)]
struct ErrorDetail<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<&'a str>,
}

impl<'a> ErrorDetail<'a> {
    fn routed(detail: &'a Detail) -> Self {
        Self {
            code: detail.code.as_deref(),
            message: &detail.message,
            node: None,
        }
    }
}

fn response_result(
    response: &Response,
    error: Option<ErrorDetail<'_>>,
) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct ResultBody<'a> {
        status: u16,
        body: &'a RawValue,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<ErrorDetail<'a>>,
    }

    serde_json::to_string(&ResultBody {
        status: response.status,
        body: serde_json::from_str(&response.body_json)?,
        error,
    })
}
