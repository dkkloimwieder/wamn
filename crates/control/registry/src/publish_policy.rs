//! Per-project publish-gate policy ([11.7], wamn-12g).
//!
//! §11.7 asks for "per-project rules (e.g. prod deploys require green suite)".
//! The registry stores those rules in two layers — an org-wide default per env
//! (`registry.env_policies.requires_green_suite`) and an optional per-project
//! override (`registry.project_publish_policies`) — and [`resolve_publish_policy`]
//! is the ONE place they are combined.
//!
//! Keeping the resolution here, in the pure registry model, is what lets the
//! `wamn-ctl copy-project-env` gate and the future authenticated management
//! transport reach the same verdict for the same `(org, project, env)` without
//! either depending on the other. A second implementation of "is this env
//! gated?" is how a control becomes bypassable.
//!
//! This module only resolves which stored policy applies. The management-owned
//! publication path is responsible for interpreting that policy.

use serde::{Deserialize, Serialize};

/// Which layer decided a [`PublishPolicy`] — recorded in the audit ledger so a
/// reviewer can tell an explicit project exemption from an org-wide default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublishPolicySource {
    /// No override row: the org's env policy answered.
    EnvDefault,
    /// A `registry.project_publish_policies` row answered for this project.
    ProjectOverride,
}

impl PublishPolicySource {
    /// The stable wire/audit spelling, matching the serde rename.
    pub fn as_str(self) -> &'static str {
        match self {
            PublishPolicySource::EnvDefault => "env-default",
            PublishPolicySource::ProjectOverride => "project-override",
        }
    }
}

/// The resolved publish-gate rule for one `(org, project, env)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PublishPolicy {
    /// Whether a definition promotion into this env requires proven-green suites.
    pub requires_green_suite: bool,
    /// Which layer supplied the answer.
    pub source: PublishPolicySource,
}

/// Combine an env's org-wide default with an optional per-project override.
///
/// The override wins in BOTH directions: a project may be held to a stricter
/// rule than its org's default, and a project may carry a recorded exemption
/// from it. An exemption is deliberately a row someone had to write — the
/// absence of a row never relaxes the env default.
pub fn resolve_publish_policy(
    env_requires_green_suite: bool,
    project_override: Option<bool>,
) -> PublishPolicy {
    match project_override {
        Some(requires_green_suite) => PublishPolicy {
            requires_green_suite,
            source: PublishPolicySource::ProjectOverride,
        },
        None => PublishPolicy {
            requires_green_suite: env_requires_green_suite,
            source: PublishPolicySource::EnvDefault,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{PublishPolicySource, resolve_publish_policy};

    #[test]
    fn absent_override_defers_to_the_env_policy() {
        for env_default in [false, true] {
            let policy = resolve_publish_policy(env_default, None);
            assert_eq!(policy.requires_green_suite, env_default);
            assert_eq!(policy.source, PublishPolicySource::EnvDefault);
        }
    }

    /// The override is authoritative in BOTH directions — a lax org may hold one
    /// project to the gate, and a strict org may record one exemption.
    #[test]
    fn project_override_wins_over_the_env_policy_in_both_directions() {
        let stricter = resolve_publish_policy(false, Some(true));
        assert!(stricter.requires_green_suite);
        assert_eq!(stricter.source, PublishPolicySource::ProjectOverride);

        let exemption = resolve_publish_policy(true, Some(false));
        assert!(!exemption.requires_green_suite);
        assert_eq!(exemption.source, PublishPolicySource::ProjectOverride);
    }

    #[test]
    fn policy_source_spellings_are_stable() {
        assert_eq!(PublishPolicySource::EnvDefault.as_str(), "env-default");
        assert_eq!(
            PublishPolicySource::ProjectOverride.as_str(),
            "project-override"
        );
    }
}
