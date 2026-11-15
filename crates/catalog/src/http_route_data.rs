//! Release data read by the separate HTTP component through wasi:config.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDefinition {
    pub attachment_id: String,
    pub path: String,
    pub method: String,
    pub body_limit: u64,
    pub cache_control: Option<String>,
    pub sealed_operation_id: String,
    pub idempotency_rule: Option<String>,
    pub etag_inputs: Option<EtagInputs>,
    pub operation_deadline_ms: u64,
    pub read_limits: Option<ReadLimits>,
    pub auth_policy: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub schema: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EtagType {
    Get(String),
    List(Vec<Relation>),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EtagInputs {
    pub release: String,
    pub type_: EtagType,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BandStart {
    pub field: String,
    pub required: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadLimits {
    pub page_minimum: Option<u32>,
    pub page_maximum: Option<u32>,
    pub band_starts: Vec<BandStart>,
}
