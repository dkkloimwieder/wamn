//! Package attachment declarations and their authentication policy.

use crate::canonical_name::CanonicalName;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An attachment exposure type.
///
/// `Deserialize` is derived so the attachment wire shape can
/// name this type instead of re-declaring one; the enum is fieldless, so the
/// derive adds no unvalidated construction path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttachmentType {
    Http,
    Internal,
    Studio,
}

/// The attachment auth-policy mode that permits an unauthenticated caller.
pub const NO_AUTHENTICATION_MODE: &str = "none";

/// The attachment auth-policy mode that requires a platform access token.
pub const PAT_AUTHENTICATION_MODE: &str = "pat";

/// The attachment auth-policy mode that permits a verified session token.
pub const SESSION_AUTHENTICATION_MODE: &str = "session";

/// The admitted authentication modes for one release attachment.
///
/// This parsed view is not a second wire representation. The manifest retains
/// the original JSON, including the canonical order of a combined modes list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentAuthPolicy<'a> {
    None,
    Pat,
    Session,
    PatAndSession,
    Signature {
        header: &'a str,
        credential: &'a str,
    },
}

impl AttachmentAuthPolicy<'_> {
    /// Whether this policy admits a request without a caller principal.
    pub fn has_no_caller(self) -> bool {
        matches!(self, Self::None | Self::Signature { .. })
    }

    /// Whether this attachment admits a platform access token.
    pub fn allows_pat(self) -> bool {
        matches!(self, Self::Pat | Self::PatAndSession)
    }

    /// Whether this attachment admits a verified session token.
    pub fn allows_session(self) -> bool {
        matches!(self, Self::Session | Self::PatAndSession)
    }
}

/// Parse the exact attachment authentication policy.
///
/// The only shapes are `{"modes":["none"]}`, `{"modes":["pat"]}`,
/// `{"modes":["session"]}`, `{"modes":["pat","session"]}`, and
/// `{"modes":["signature"],"header":"…","credential":"…"}`. Unknown
/// fields, scalar modes, duplicates, and other list orders are refused.
pub fn parse_attachment_auth_policy(policy: &Value) -> Option<AttachmentAuthPolicy<'_>> {
    let object = policy.as_object()?;
    let modes = object.get("modes")?.as_array()?;
    if matches!(modes.as_slice(), [mode] if mode.as_str() == Some("signature")) {
        if object.len() != 3 {
            return None;
        }
        let header = object.get("header")?.as_str()?;
        let credential = object.get("credential")?.as_str()?;
        return (!header.is_empty() && !credential.is_empty())
            .then_some(AttachmentAuthPolicy::Signature { header, credential });
    }
    if object.len() != 1 {
        return None;
    }
    match modes.as_slice() {
        [mode] => match mode.as_str()? {
            NO_AUTHENTICATION_MODE => Some(AttachmentAuthPolicy::None),
            PAT_AUTHENTICATION_MODE => Some(AttachmentAuthPolicy::Pat),
            SESSION_AUTHENTICATION_MODE => Some(AttachmentAuthPolicy::Session),
            _ => None,
        },
        [pat, session]
            if pat.as_str() == Some(PAT_AUTHENTICATION_MODE)
                && session.as_str() == Some(SESSION_AUTHENTICATION_MODE) =>
        {
            Some(AttachmentAuthPolicy::PatAndSession)
        }
        _ => None,
    }
}

/// Stable refusal literal for malformed or unsupported attachment auth policy.
pub const INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL: &str = "invalid-attachment-auth-policy";

/// What one release attachment invokes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentTarget {
    /// One component export of the attachment's package, through a route.
    Route {
        component: CanonicalName,
        operation: CanonicalName,
    },
}

/// One authored attachment targeting a route.
///
/// Codegen stores these route facts in the package's compiled deploy section.
/// Edge assembly converts them into a release routes.
///
/// On the wire the target is flat: `component` and `operation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AttachmentWire", into = "AttachmentWire")]
pub struct ServingAttachment {
    pub type_: AttachmentType,
    pub package_id: crate::PackageName,
    pub target: AttachmentTarget,
    pub definition_hash: wamn_digest::Digest,
    pub definition: Value,
    pub auth_policy: Value,
    /// Exact operation authority selected by this attachment. Attachments that
    /// do not invoke a package operation carry no token; callers never infer one
    /// from route, wiring, or component syntax.
    pub registered_operation: Option<crate::registered_operation::RegisteredOperation>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct AttachmentWire {
    #[serde(rename = "type")]
    type_: AttachmentType,
    package_id: crate::PackageName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    component: Option<CanonicalName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation: Option<CanonicalName>,
    definition_hash: wamn_digest::Digest,
    definition: Value,
    auth_policy: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registered_operation: Option<crate::registered_operation::RegisteredOperation>,
}

impl TryFrom<AttachmentWire> for ServingAttachment {
    type Error = String;

    fn try_from(wire: AttachmentWire) -> Result<Self, Self::Error> {
        let target = AttachmentTarget::Route {
            component: wire.component.ok_or("attachment component is required")?,
            operation: wire.operation.ok_or("attachment operation is required")?,
        };
        Ok(Self {
            type_: wire.type_,
            package_id: wire.package_id,
            target,
            definition_hash: wire.definition_hash,
            definition: wire.definition,
            auth_policy: wire.auth_policy,
            registered_operation: wire.registered_operation,
        })
    }
}

impl From<ServingAttachment> for AttachmentWire {
    fn from(attachment: ServingAttachment) -> Self {
        let AttachmentTarget::Route {
            component,
            operation,
        } = attachment.target;
        Self {
            type_: attachment.type_,
            package_id: attachment.package_id,
            component: Some(component),
            operation: Some(operation),
            definition_hash: attachment.definition_hash,
            definition: attachment.definition,
            auth_policy: attachment.auth_policy,
            registered_operation: attachment.registered_operation,
        }
    }
}
