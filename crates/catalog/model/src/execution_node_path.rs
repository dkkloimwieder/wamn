use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use schemars::schema::{ArrayValidation, Schema, SingleOrVec, StringValidation};
use serde::{Deserialize, Deserializer, Serialize};

const PATH_SEPARATOR: &str = "/";
const SEGMENT_PATTERN: &str = "^[a-z0-9-]+$";

/// An internal node location within one fully expanded execution plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ExecutionNodePath(Box<[String]>);

/// A value cannot form a canonical [`ExecutionNodePath`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionNodePathError {
    segment_index: Option<usize>,
    segment: Option<String>,
}

impl ExecutionNodePathError {
    fn empty_path() -> Self {
        Self {
            segment_index: None,
            segment: None,
        }
    }

    fn invalid_segment(segment_index: usize, segment: String) -> Self {
        Self {
            segment_index: Some(segment_index),
            segment: Some(segment),
        }
    }

    /// Whether the rejected value contained no path segments.
    pub fn is_empty_path(&self) -> bool {
        self.segment_index.is_none()
    }

    /// The zero-based index of the rejected segment, when one was present.
    pub fn segment_index(&self) -> Option<usize> {
        self.segment_index
    }

    /// The rejected segment, when one was present.
    pub fn segment(&self) -> Option<&str> {
        self.segment.as_deref()
    }
}

impl fmt::Display for ExecutionNodePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.segment_index, self.segment.as_deref()) {
            (Some(index), Some(segment)) => write!(
                formatter,
                "execution node path segment {index} {segment:?} must match {SEGMENT_PATTERN}"
            ),
            _ => formatter.write_str("execution node path must contain at least one segment"),
        }
    }
}

impl std::error::Error for ExecutionNodePathError {}

impl ExecutionNodePath {
    /// Validate an ordered, nonempty sequence of node-id segments.
    pub fn new(segments: Vec<String>) -> Result<Self, ExecutionNodePathError> {
        if segments.is_empty() {
            return Err(ExecutionNodePathError::empty_path());
        }

        for (index, segment) in segments.iter().enumerate() {
            if !valid_segment(segment) {
                return Err(ExecutionNodePathError::invalid_segment(
                    index,
                    segment.clone(),
                ));
            }
        }

        Ok(Self(segments.into_boxed_slice()))
    }

    /// Return the ordered node-id segments.
    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Display for ExecutionNodePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str(PATH_SEPARATOR)?;
            }
            formatter.write_str(segment)?;
        }
        Ok(())
    }
}

impl FromStr for ExecutionNodePath {
    type Err = ExecutionNodePathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Self::new(Vec::new());
        }
        Self::new(value.split(PATH_SEPARATOR).map(str::to_owned).collect())
    }
}

impl<'de> Deserialize<'de> for ExecutionNodePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let segments = Vec::<String>::deserialize(deserializer)?;
        Self::new(segments).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ExecutionNodePath {
    fn schema_name() -> String {
        "ExecutionNodePath".to_owned()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> Schema {
        let mut segment_schema = <String as JsonSchema>::json_schema(generator).into_object();
        segment_schema.string = Some(Box::new(StringValidation {
            pattern: Some(SEGMENT_PATTERN.to_owned()),
            ..StringValidation::default()
        }));

        let mut path_schema = <Vec<String> as JsonSchema>::json_schema(generator).into_object();
        path_schema.array = Some(Box::new(ArrayValidation {
            items: Some(SingleOrVec::Single(Box::new(Schema::Object(
                segment_schema,
            )))),
            min_items: Some(1),
            ..ArrayValidation::default()
        }));
        Schema::Object(path_schema)
    }
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
