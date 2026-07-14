//! Tier-move (promotion) planning — T3→T2 (trial-convert) and T2→T4 (regulated
//! upgrade) (wamn-q3n.13).
//!
//! A tier move is a **scheduled, one-way promotion**: it re-points an org onto a
//! higher-isolation tier's clusters by, per project-env, dumping the current
//! database, provisioning it on the new cluster, restoring the dump, then flipping
//! the registry placement row — the `CredentialProvider` seam
//! (docs/postgres-topology.md §Reversibility). It is **not free at the data
//! layer**: a dump/restore window (or a logical-replication cutover for
//! near-zero-downtime); promotions are scheduled operations, not no-ops.
//!
//! This module is the **pure** core (SR6 rule 1): the upgrade-path validation and
//! the ordered step plan — no DB, clock, or K8s client. The effects live in the
//! `move-org-tier` subcommand (`wamn-host`), which reuses the built pieces —
//! `provision-org` (.6), `provision-project-env` (.7), `dump-project-env` (.10),
//! and `restore-project-env` (.11). The resumable/compensating saga that would
//! drive the plan automatically is `10.1`'s; `.13` ships the mechanism + runbook.

use wamn_registry::{Env, Org, Tier, Triple};

use crate::error::ProvisionError;

/// A tier's isolation rank — the upgrade lattice `trials < standard < dedicated`
/// (ascending isolation, the [`Tier::ALL`](wamn_registry::Tier::ALL) order). A move
/// is valid only if it strictly increases the rank.
fn tier_rank(tier: Tier) -> u8 {
    match tier {
        Tier::Trials => 0,
        Tier::Standard => 1,
        Tier::Dedicated => 2,
    }
}

/// Validate a tier move is a strict **upgrade** (higher isolation). A same-tier
/// move is a no-op ([`ProvisionError::TierMoveNoop`]); a downgrade is unsupported
/// ([`ProvisionError::TierDowngrade`]) — data never moves *down* to a shared or
/// lower-isolation tier. Both directions the platform supports (T3→T2, T2→T4) are
/// upgrades.
pub fn validate_tier_upgrade(current: Tier, target: Tier) -> Result<(), ProvisionError> {
    let (c, t) = (tier_rank(current), tier_rank(target));
    if t > c {
        Ok(())
    } else if t == c {
        Err(ProvisionError::TierMoveNoop {
            tier: current.as_str(),
        })
    } else {
        Err(ProvisionError::TierDowngrade {
            from: current.as_str(),
            to: target.as_str(),
        })
    }
}

/// One ordered step of a tier move — the scheduled runbook an operator (or `10.1`'s
/// saga) executes. Each maps to an existing subcommand; the **sequence** is the
/// contribution (dump before flip; restore before the registry cutover).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierMoveStep {
    /// Provision + wait-ready the target tier's new cluster pair (`provision-org`,
    /// render-only — the flip is deferred to the final [`FlipRegistry`] step).
    ProvisionClusters {
        prod_cluster: String,
        dev_cluster: String,
        prod_instances: u32,
    },
    /// Snapshot one project-env's CURRENT database (`dump-project-env --run-now`).
    Dump { triple: Triple },
    /// Create the project-env database on the NEW cluster (`provision-project-env
    /// --cluster <new>`), then restore into it.
    ProvisionEnv { triple: Triple, cluster: String },
    /// Restore the snapshot into the new database (`restore-project-env --in-place`).
    Restore { triple: Triple, cluster: String },
    /// Flip the org's registry row to the new tier + cluster refs — the cutover
    /// (`upsert_org_sql`), **LAST** so the control-plane placement follows the data
    /// move (a losing/early flip would route live traffic before the data is there).
    FlipRegistry {
        tier: Tier,
        prod_cluster: String,
        dev_cluster: String,
    },
}

/// Plan an org's move to `target` tier: provision the new clusters, then per
/// project-env `dump → provision-on-new → restore`, then flip the registry
/// **last**. `project_envs` are the org's `(project, env)` rows (the caller reads
/// them from the registry). Errors if the move is not a strict upgrade.
///
/// The target's cluster names come from [`Org::for_pair`] (the single-source
/// `cluster_name` the CR renderer and the flipped registry row also use), so a
/// planned move, the provisioned clusters, and the flipped row all name the same
/// clusters — what [`resolve`](wamn_registry::Registry::resolve) relies on.
pub fn plan_tier_move(
    org: &str,
    current: Tier,
    target: Tier,
    project_envs: &[(String, Env)],
) -> Result<Vec<TierMoveStep>, ProvisionError> {
    validate_tier_upgrade(current, target)?;
    let target_org = Org::for_pair(org, target);
    let prod = target_org.prod_cluster.name.clone();
    let dev = target_org.dev_cluster.name.clone();

    let mut steps = vec![TierMoveStep::ProvisionClusters {
        prod_cluster: prod.clone(),
        dev_cluster: dev.clone(),
        prod_instances: crate::org::prod_instances(target).unwrap_or(1),
    }];
    for (project, env) in project_envs {
        let triple = Triple::new(org, project.as_str(), *env);
        // The env's recovery-domain side picks which of the new pair holds it
        // (prod/canary → <org>-prod; dev → <org>-dev).
        let cluster = target_org.cluster(env.side()).name.clone();
        steps.push(TierMoveStep::Dump {
            triple: triple.clone(),
        });
        steps.push(TierMoveStep::ProvisionEnv {
            triple: triple.clone(),
            cluster: cluster.clone(),
        });
        steps.push(TierMoveStep::Restore { triple, cluster });
    }
    steps.push(TierMoveStep::FlipRegistry {
        tier: target,
        prod_cluster: prod,
        dev_cluster: dev,
    });
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a strict upgrade is allowed: both supported directions (T3→T2, T2→T4,
    /// and the T3→T4 shortcut) pass; a same-tier move is a no-op and any downgrade
    /// is rejected. The upgrade lattice is `trials < standard < dedicated`.
    #[test]
    fn only_a_strict_upgrade_is_allowed() {
        // Upgrades.
        assert!(validate_tier_upgrade(Tier::Trials, Tier::Standard).is_ok());
        assert!(validate_tier_upgrade(Tier::Standard, Tier::Dedicated).is_ok());
        assert!(validate_tier_upgrade(Tier::Trials, Tier::Dedicated).is_ok());
        // No-ops (same tier).
        assert!(matches!(
            validate_tier_upgrade(Tier::Standard, Tier::Standard),
            Err(ProvisionError::TierMoveNoop { tier: "standard" })
        ));
        assert!(matches!(
            validate_tier_upgrade(Tier::Trials, Tier::Trials),
            Err(ProvisionError::TierMoveNoop { tier: "trials" })
        ));
        // Downgrades.
        assert!(matches!(
            validate_tier_upgrade(Tier::Dedicated, Tier::Standard),
            Err(ProvisionError::TierDowngrade {
                from: "dedicated",
                to: "standard"
            })
        ));
        assert!(matches!(
            validate_tier_upgrade(Tier::Standard, Tier::Trials),
            Err(ProvisionError::TierDowngrade { .. })
        ));
    }

    /// The plan provisions clusters FIRST, dumps→provisions→restores each env, and
    /// flips the registry LAST — and each env routes to the correct side of the new
    /// pair (prod → `<org>-prod`, dev → `<org>-dev`).
    #[test]
    fn plan_orders_provision_then_per_env_then_flip_last() {
        let envs = vec![
            ("app".to_string(), Env::Prod),
            ("app".to_string(), Env::Dev),
        ];
        let steps = plan_tier_move("acme", Tier::Trials, Tier::Standard, &envs).unwrap();

        // First: provision the new pair (standard prod = 2 instances).
        assert_eq!(
            steps[0],
            TierMoveStep::ProvisionClusters {
                prod_cluster: "acme-prod".into(),
                dev_cluster: "acme-dev".into(),
                prod_instances: 2,
            }
        );
        // Last: the registry cutover, naming the new tier + clusters.
        assert_eq!(
            *steps.last().unwrap(),
            TierMoveStep::FlipRegistry {
                tier: Tier::Standard,
                prod_cluster: "acme-prod".into(),
                dev_cluster: "acme-dev".into(),
            }
        );
        // Each env: dump → provision-on-new → restore, routed by env side.
        assert_eq!(
            &steps[1..7],
            &[
                TierMoveStep::Dump {
                    triple: Triple::new("acme", "app", Env::Prod)
                },
                TierMoveStep::ProvisionEnv {
                    triple: Triple::new("acme", "app", Env::Prod),
                    cluster: "acme-prod".into(),
                },
                TierMoveStep::Restore {
                    triple: Triple::new("acme", "app", Env::Prod),
                    cluster: "acme-prod".into(),
                },
                TierMoveStep::Dump {
                    triple: Triple::new("acme", "app", Env::Dev)
                },
                TierMoveStep::ProvisionEnv {
                    triple: Triple::new("acme", "app", Env::Dev),
                    cluster: "acme-dev".into(),
                },
                TierMoveStep::Restore {
                    triple: Triple::new("acme", "app", Env::Dev),
                    cluster: "acme-dev".into(),
                },
            ]
        );
    }

    /// T2→T4 (standard → dedicated) is the same code path, with a 3-instance prod
    /// cluster (the dedicated HA shape).
    #[test]
    fn t2_to_t4_provisions_a_three_instance_prod() {
        let envs = vec![("app".to_string(), Env::Prod)];
        let steps = plan_tier_move("acme", Tier::Standard, Tier::Dedicated, &envs).unwrap();
        assert_eq!(
            steps[0],
            TierMoveStep::ProvisionClusters {
                prod_cluster: "acme-prod".into(),
                dev_cluster: "acme-dev".into(),
                prod_instances: 3,
            }
        );
    }

    /// A downgrade produces no plan at all (the validation gate short-circuits).
    #[test]
    fn plan_refuses_a_downgrade() {
        let envs = vec![("app".to_string(), Env::Prod)];
        assert!(plan_tier_move("acme", Tier::Standard, Tier::Trials, &envs).is_err());
    }
}
