//! Request and response bodies and routes for NATS signing.

use serde::{Deserialize, Serialize};
use wamn_registry::{EnvName, OrgId, ProjectId};

/// Account provisioning and resolver publication route.
pub const ENSURE_ACCOUNT_PATH: &str = "/nats/accounts";
/// User JWT signing route.
pub const SIGN_USER_PATH: &str = "/nats/users";
/// Scheduler JWT signing route for a host pod.
pub const SIGN_HOST_USER_PATH: &str = "/nats/host-user";

/// Ensure one environment account with a disk reservation for its project count.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsureAccount {
    pub org: OrgId,
    pub env: EnvName,
    /// A zero count removes the account from the full resolver.
    pub project_count: u32,
}

/// The environment account that the NATS resolver accepted.
#[derive(Debug, Serialize, Deserialize)]
pub struct Account {
    pub public_key: String,
}

/// Select the permissions identity grants to the user's public key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UserPurpose {
    Workflow,
    Ingress,
    WorkflowNative,
    Cdc { project: ProjectId },
    Recovery { project: ProjectId },
    StreamAdmin,
    HostData,
}

/// Sign a NATS user for the authenticated org. The caller retains the seed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignUser {
    pub org: OrgId,
    pub env: EnvName,
    #[serde(rename = "kind")]
    pub purpose: UserPurpose,
    pub public_key: String,
}

/// Sign a scheduler user for the pod identified by its service account token.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignHostUser {
    pub public_key: String,
}

/// The signed user JWT and the inbox prefix the client must use.
#[derive(Serialize, Deserialize)]
pub struct SignedUser {
    pub jwt: String,
    pub inbox_prefix: String,
}

impl std::fmt::Debug for SignedUser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SignedUser").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{SignUser, UserPurpose};

    #[test]
    fn user_signing_preserves_the_wire_field() {
        let request = SignUser {
            org: "acme".parse().unwrap(),
            env: "test".parse().unwrap(),
            purpose: UserPurpose::Cdc {
                project: "inventory".parse().unwrap(),
            },
            public_key: "public-key".into(),
        };
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(
            wire["kind"],
            serde_json::json!({"type": "cdc", "project": "inventory"})
        );
        let decoded: SignUser = serde_json::from_value(wire).unwrap();
        assert!(matches!(decoded.purpose, UserPurpose::Cdc { .. }));
    }
}
