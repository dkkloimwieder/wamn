//! Versioned trusted identity carried from admission into effect calls.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only invocation-context document version currently accepted.
pub const INVOCATION_CONTEXT_VERSION: &str = "0.1";

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

/// The trusted frame facts added to the admitted principal for one dispatched effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct HttpEffectPrincipal {
    root_plan_hash: String,
    current_plan_hash: String,
    frame_id: u64,
    local_node_id: String,
    occurrence: u32,
    source_artifact_hash: String,
    requirement_name: String,
}

impl HttpEffectPrincipal {
    pub fn new(
        root_plan_hash: impl Into<String>,
        current_plan_hash: impl Into<String>,
        frame_id: u64,
        local_node_id: impl Into<String>,
        occurrence: u32,
        source_artifact_hash: impl Into<String>,
        requirement_name: impl Into<String>,
    ) -> Result<Self, InvocationContextError> {
        let effect = Self {
            root_plan_hash: root_plan_hash.into(),
            current_plan_hash: current_plan_hash.into(),
            frame_id,
            local_node_id: local_node_id.into(),
            occurrence,
            source_artifact_hash: source_artifact_hash.into(),
            requirement_name: requirement_name.into(),
        };
        if !is_sha256(&effect.root_plan_hash)
            || !is_sha256(&effect.current_plan_hash)
            || !is_slug(&effect.local_node_id)
            || !is_sha256(&effect.source_artifact_hash)
            || effect.requirement_name.is_empty()
        {
            return Err(InvocationContextError::InvalidEffect);
        }
        Ok(effect)
    }

    pub fn root_plan_hash(&self) -> &str {
        &self.root_plan_hash
    }

    pub fn current_plan_hash(&self) -> &str {
        &self.current_plan_hash
    }

    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    pub fn local_node_id(&self) -> &str {
        &self.local_node_id
    }

    pub fn occurrence(&self) -> u32 {
        self.occurrence
    }

    pub fn source_artifact_hash(&self) -> &str {
        &self.source_artifact_hash
    }

    pub fn requirement_name(&self) -> &str {
        &self.requirement_name
    }
}

/// One governed context type for persisted admission and an HTTP effect call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TrustedInvocationContext {
    version: String,
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
            version: INVOCATION_CONTEXT_VERSION.to_string(),
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
        if self.http_effect.as_ref().is_some_and(|effect| {
            !is_sha256(&effect.root_plan_hash)
                || !is_sha256(&effect.current_plan_hash)
                || !is_slug(&effect.local_node_id)
                || !is_sha256(&effect.source_artifact_hash)
                || effect.requirement_name.is_empty()
        }) {
            return Err(InvocationContextError::InvalidEffect);
        }
        Ok(())
    }
}

fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
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

    fn hash(seed: char) -> String {
        format!("sha256:{seed:0<64}")
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
            HttpEffectPrincipal::new(
                hash('a'),
                hash('b'),
                2,
                "notify",
                4,
                hash('c'),
                "manager-notifications",
            )
            .unwrap(),
        );
        assert_eq!(effect.principal().artifact_digest(), "sha256:artifact");
        assert_eq!(effect.http_effect().unwrap().root_plan_hash(), hash('a'));
        assert_eq!(effect.http_effect().unwrap().current_plan_hash(), hash('b'));
        assert_eq!(effect.http_effect().unwrap().frame_id(), 2);
        assert_eq!(effect.http_effect().unwrap().local_node_id(), "notify");
        assert_eq!(effect.http_effect().unwrap().occurrence(), 4);
        assert_eq!(
            effect.http_effect().unwrap().source_artifact_hash(),
            hash('c')
        );
        assert_eq!(
            effect.http_effect().unwrap().requirement_name(),
            "manager-notifications"
        );
    }

    #[test]
    fn dispatched_effect_context_round_trips_exactly_seven_trusted_facts() {
        let effect = HttpEffectPrincipal::new(
            hash('a'),
            hash('b'),
            0,
            "notify",
            3,
            hash('c'),
            "manager-notifications",
        )
        .unwrap();
        let object = serde_json::to_value(&effect).unwrap();
        let keys = object.as_object().unwrap();
        assert_eq!(keys.len(), 7, "{object}");
        for key in [
            "root-plan-hash",
            "current-plan-hash",
            "frame-id",
            "local-node-id",
            "occurrence",
            "source-artifact-hash",
            "requirement-name",
        ] {
            assert!(keys.contains_key(key), "missing {key}: {object}");
        }
        for residue in ["attempt", "execution-bundle-hash"] {
            assert!(!keys.contains_key(residue), "retained old fact {residue}");
        }
        assert_eq!(
            serde_json::from_value::<HttpEffectPrincipal>(object).unwrap(),
            effect
        );
    }

    #[test]
    fn dispatched_effect_context_rejects_old_and_unknown_shapes() {
        let effect = HttpEffectPrincipal::new(
            hash('a'),
            hash('b'),
            0,
            "notify",
            3,
            hash('c'),
            "manager-notifications",
        )
        .unwrap();

        let mut old_shape = serde_json::to_value(&effect).unwrap();
        old_shape.as_object_mut().unwrap().remove("occurrence");
        assert!(serde_json::from_value::<HttpEffectPrincipal>(old_shape).is_err());

        let mut unknown_shape = serde_json::to_value(&effect).unwrap();
        unknown_shape
            .as_object_mut()
            .unwrap()
            .insert("attempt".to_string(), json!(0));
        assert!(serde_json::from_value::<HttpEffectPrincipal>(unknown_shape).is_err());
    }

    #[test]
    fn dispatched_effect_context_rejects_noncanonical_frame_facts() {
        for (local_node_id, source_artifact_hash, requirement_name) in [
            ("Notify", hash('c'), "manager-notifications".to_string()),
            (
                "notify_node",
                hash('c'),
                "manager-notifications".to_string(),
            ),
            (
                "notify",
                "sha256:SOURCE".to_string(),
                "manager-notifications".to_string(),
            ),
            (
                "notify",
                "sha256:short".to_string(),
                "manager-notifications".to_string(),
            ),
            ("notify", hash('c'), String::new()),
        ] {
            assert!(
                HttpEffectPrincipal::new(
                    hash('a'),
                    hash('b'),
                    0,
                    local_node_id,
                    0,
                    source_artifact_hash,
                    requirement_name,
                )
                .is_err(),
                "accepted noncanonical effect facts"
            );
        }
    }

    #[test]
    fn unversioned_incomplete_and_unknown_shapes_fail_closed() {
        for invalid in [
            json!({}),
            json!({
                "version": 1,
                "principal": serde_json::to_value(principal()).unwrap(),
                "source": {}
            }),
            json!({
                "version": "unsupported",
                "principal": serde_json::to_value(principal()).unwrap(),
                "source": {}
            }),
            json!({
                "version": "0.1",
                "principal": serde_json::to_value(principal()).unwrap(),
                "source": {},
                "permission": "all-http"
            }),
        ] {
            assert!(TrustedInvocationContext::from_json(&invalid.to_string()).is_err());
        }
    }
}
