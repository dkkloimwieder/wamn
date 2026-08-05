//! Versioned trusted identity carried from admission into effect calls.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only invocation-context document version currently accepted.
pub const INVOCATION_CONTEXT_VERSION: u32 = 1;

/// Release and artifact identity decided by admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedPrincipal {
    tenant_id: String,
    environment: String,
    catalog_id: String,
    catalog_version: i32,
    run_id: String,
    flow_id: String,
    flow_version: u32,
    artifact_digest: String,
}

impl AdmittedPrincipal {
    /// Construct the principal derived from one admitted release artifact.
    #[expect(
        clippy::too_many_arguments,
        reason = "the admission trust boundary must name every principal field explicitly"
    )]
    pub fn new(
        tenant_id: impl Into<String>,
        environment: impl Into<String>,
        catalog_id: impl Into<String>,
        catalog_version: i32,
        run_id: impl Into<String>,
        flow_id: impl Into<String>,
        flow_version: u32,
        artifact_digest: impl Into<String>,
    ) -> Result<Self, InvocationContextError> {
        let principal = Self {
            tenant_id: tenant_id.into(),
            environment: environment.into(),
            catalog_id: catalog_id.into(),
            catalog_version,
            run_id: run_id.into(),
            flow_id: flow_id.into(),
            flow_version,
            artifact_digest: artifact_digest.into(),
        };
        principal.validate()?;
        Ok(principal)
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    pub fn catalog_version(&self) -> i32 {
        self.catalog_version
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    pub fn flow_version(&self) -> u32 {
        self.flow_version
    }

    pub fn artifact_digest(&self) -> &str {
        &self.artifact_digest
    }

    fn validate(&self) -> Result<(), InvocationContextError> {
        if self.tenant_id.is_empty()
            || self.environment.is_empty()
            || self.catalog_id.is_empty()
            || self.run_id.is_empty()
            || self.flow_id.is_empty()
            || self.artifact_digest.is_empty()
            || self.catalog_version <= 0
            || self.flow_version == 0
        {
            return Err(InvocationContextError::InvalidPrincipal);
        }
        Ok(())
    }
}

/// The per-attempt tail added to the admitted principal for one HTTP effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct HttpEffectPrincipal {
    node_id: String,
    occurrence: u32,
    attempt: u32,
    requirement_name: String,
}

impl HttpEffectPrincipal {
    pub fn new(
        node_id: impl Into<String>,
        occurrence: u32,
        attempt: u32,
        requirement_name: impl Into<String>,
    ) -> Result<Self, InvocationContextError> {
        let effect = Self {
            node_id: node_id.into(),
            occurrence,
            attempt,
            requirement_name: requirement_name.into(),
        };
        if effect.node_id.is_empty() || effect.requirement_name.is_empty() {
            return Err(InvocationContextError::InvalidEffect);
        }
        Ok(effect)
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn requirement_name(&self) -> &str {
        &self.requirement_name
    }
}

/// One governed context type for persisted admission and an HTTP effect call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TrustedInvocationContext {
    version: u32,
    principal: AdmittedPrincipal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http_effect: Option<HttpEffectPrincipal>,
    source: Value,
}

impl TrustedInvocationContext {
    /// Wrap producer-specific metadata with the principal derived by admission.
    pub fn admitted(
        principal: AdmittedPrincipal,
        source: Value,
    ) -> Result<Self, InvocationContextError> {
        if !source.is_object() {
            return Err(InvocationContextError::InvalidSource);
        }
        Ok(Self {
            version: INVOCATION_CONTEXT_VERSION,
            principal,
            http_effect: None,
            source,
        })
    }

    /// Derive the same context's complete per-attempt HTTP principal.
    pub fn with_http_effect(mut self, effect: HttpEffectPrincipal) -> Self {
        self.http_effect = Some(effect);
        self
    }

    /// Decode and validate persisted context bytes fail-closed.
    pub fn from_json(json: &str) -> Result<Self, InvocationContextError> {
        let context: Self =
            serde_json::from_str(json).map_err(|_| InvocationContextError::InvalidDocument)?;
        context.validate()?;
        Ok(context)
    }

    pub fn principal(&self) -> &AdmittedPrincipal {
        &self.principal
    }

    pub fn http_effect(&self) -> Option<&HttpEffectPrincipal> {
        self.http_effect.as_ref()
    }

    pub fn source(&self) -> &Value {
        &self.source
    }

    fn validate(&self) -> Result<(), InvocationContextError> {
        if self.version != INVOCATION_CONTEXT_VERSION {
            return Err(InvocationContextError::UnsupportedVersion);
        }
        self.principal.validate()?;
        if !self.source.is_object() {
            return Err(InvocationContextError::InvalidSource);
        }
        if self
            .http_effect
            .as_ref()
            .is_some_and(|effect| effect.node_id.is_empty() || effect.requirement_name.is_empty())
        {
            return Err(InvocationContextError::InvalidEffect);
        }
        Ok(())
    }
}

/// A trusted invocation context could not be constructed or decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationContextError {
    InvalidDocument,
    UnsupportedVersion,
    InvalidPrincipal,
    InvalidEffect,
    InvalidSource,
}

impl std::fmt::Display for InvocationContextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDocument => "trusted invocation context is not the governed shape",
            Self::UnsupportedVersion => "trusted invocation context version is unsupported",
            Self::InvalidPrincipal => "trusted invocation principal is incomplete",
            Self::InvalidEffect => "trusted HTTP effect principal is incomplete",
            Self::InvalidSource => "trusted invocation source metadata must be an object",
        })
    }
}

impl std::error::Error for InvocationContextError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn principal() -> AdmittedPrincipal {
        AdmittedPrincipal::new(
            "tenant-a",
            "prod",
            "catalog-a",
            7,
            "run-a",
            "flow-a",
            3,
            "sha256:artifact",
        )
        .unwrap()
    }

    #[test]
    fn admitted_context_round_trips_then_becomes_one_effect_call_frame() {
        let admitted = TrustedInvocationContext::admitted(
            principal(),
            json!({"trigger": "cron", "scheduled-at": "2026-08-04T20:00:00Z"}),
        )
        .unwrap();
        let persisted = serde_json::to_string(&admitted).unwrap();
        let decoded = TrustedInvocationContext::from_json(&persisted).unwrap();
        assert_eq!(decoded, admitted);
        assert!(decoded.http_effect().is_none());

        let effect = decoded.with_http_effect(
            HttpEffectPrincipal::new("notify", 2, 1, "manager-notifications").unwrap(),
        );
        assert_eq!(effect.principal().artifact_digest(), "sha256:artifact");
        assert_eq!(effect.http_effect().unwrap().node_id(), "notify");
        assert_eq!(effect.http_effect().unwrap().occurrence(), 2);
        assert_eq!(effect.http_effect().unwrap().attempt(), 1);
        assert_eq!(
            effect.http_effect().unwrap().requirement_name(),
            "manager-notifications"
        );
    }

    #[test]
    fn unversioned_incomplete_and_unknown_shapes_fail_closed() {
        for invalid in [
            json!({}),
            json!({
                "version": 2,
                "principal": serde_json::to_value(principal()).unwrap(),
                "source": {}
            }),
            json!({
                "version": 1,
                "principal": serde_json::to_value(principal()).unwrap(),
                "source": {},
                "permission": "all-http"
            }),
        ] {
            assert!(TrustedInvocationContext::from_json(&invalid.to_string()).is_err());
        }
    }
}
