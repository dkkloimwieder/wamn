//! Canonical portable HTTP request-target normalization.

/// A portable HTTP target normalized exactly once for canonical consumers.
///
/// The private field prevents authority resolvers from accepting a target that
/// bypassed [`normalize_portable_http_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalHttpTarget(Box<str>);

impl CanonicalHttpTarget {
    /// Return the connection-relative spelling consumed by authority resolvers.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Normalize the sole portable spelling by stripping exactly one leading `/`.
pub fn normalize_portable_http_target(
    portable: &str,
) -> Result<CanonicalHttpTarget, PortableHttpTargetError> {
    if portable.starts_with("//") {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query cannot start with //",
        ));
    }
    let Some(canonical) = portable.strip_prefix('/') else {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query must start with /",
        ));
    };
    if canonical.is_empty() {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query must contain a path",
        ));
    }
    let first_segment = canonical.split(['/', '?', '#']).next().unwrap_or_default();
    if canonical.contains(['#', '\\']) || first_segment.contains(':') {
        return Err(PortableHttpTargetError::new(
            "portable HTTP path-and-query is ambiguous",
        ));
    }
    Ok(CanonicalHttpTarget(canonical.into()))
}

/// A portable HTTP target that cannot enter canonical resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableHttpTargetError {
    detail: Box<str>,
}

impl PortableHttpTargetError {
    fn new(detail: impl Into<Box<str>>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for PortableHttpTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for PortableHttpTargetError {}
