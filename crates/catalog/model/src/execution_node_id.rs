//! Scalar node identity within one execution plan.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use schemars::schema::{Schema, StringValidation};
use serde::{Deserialize, Deserializer, Serialize};

const NODE_ID_PATTERN: &str = "^[a-z0-9-]+$";

/// A scalar node identifier within one execution plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ExecutionNodeId(Box<str>);

/// A value cannot form an [`ExecutionNodeId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionNodeIdError {
    value: Box<str>,
}

impl ExecutionNodeIdError {
    fn new(value: String) -> Self {
        Self {
            value: value.into_boxed_str(),
        }
    }

    /// Return the rejected value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ExecutionNodeIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "execution node id {:?} must match {NODE_ID_PATTERN}",
            self.value
        )
    }
}

impl std::error::Error for ExecutionNodeIdError {}

impl ExecutionNodeId {
    /// Validate one scalar execution node identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, ExecutionNodeIdError> {
        let value = value.into();
        if !valid_node_id(&value) {
            return Err(ExecutionNodeIdError::new(value));
        }
        Ok(Self(value.into_boxed_str()))
    }

    /// Return the scalar node identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ExecutionNodeId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ExecutionNodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ExecutionNodeId {
    type Err = ExecutionNodeIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ExecutionNodeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ExecutionNodeId {
    fn schema_name() -> String {
        "ExecutionNodeId".to_owned()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> Schema {
        let mut schema = <String as JsonSchema>::json_schema(generator).into_object();
        schema.string = Some(Box::new(StringValidation {
            pattern: Some(NODE_ID_PATTERN.to_owned()),
            ..StringValidation::default()
        }));
        Schema::Object(schema)
    }
}

fn valid_node_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
