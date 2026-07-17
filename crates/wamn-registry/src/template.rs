//! Named org templates — the `Tier` successor (wamn-8df.4).
//!
//! A [`Template`] is a one-click preset that **stamps** an org's [`Placement`]
//! plus its initial per-org [`EnvPolicy`] set in one step, replacing the retired
//! closed `Tier` enum (`docs/deployment-model.md` §"The four tiers survive as
//! configurations"). The three shipped presets re-provide the old tiers as data:
//!
//! | Template | Old tier | Placement | Policy set |
//! |---|---|---|---|
//! | `trials` | T3 | pooled (shares `--pool`) | `dev`, `prod` |
//! | `standard` | T2 | dedicated | `dev`(own), `canary`(shared-with `prod`), `prod`(own) |
//! | `dedicated` | T4 | dedicated | `dev`(own), `canary`(**own**), `prod`(own) |
//!
//! Stamping is **instantiate-and-own**: the org gets its own copy of the policy
//! rows (insert-if-absent — `sql::stamp_env_policy_sql`), which it then
//! customizes per-env without touching any other org or any later change to the
//! template. Re-running `provision-org` with a richer template adds the missing
//! envs and keeps existing customizations; a template edit never silently
//! resizes an already-provisioned customer.
//!
//! Templates are **code presets** (versioned, drift-guarded with the model), not
//! registry rows — a template is consulted only at stamp time.

use crate::types::{Env, EnvPolicy, Org, OrgEnvPolicy, Placement, RecoveryDomain};

/// A named org preset: a placement shape plus the env-policy set it stamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// The preset's name (`trials` / `standard` / `dedicated`).
    pub name: &'static str,
    /// Whether stamped orgs share a pool (the pool name is the instantiator's,
    /// e.g. `provision-org --pool`) or own per-recovery-domain clusters.
    pub pooled: bool,
    /// The env-policy set stamped for the org (its initial per-org copy).
    pub policies: Vec<EnvPolicy>,
}

impl Template {
    /// The shipped preset names, in ascending isolation order.
    pub const NAMES: [&'static str; 3] = ["trials", "standard", "dedicated"];

    /// Look a shipped preset up by name.
    pub fn by_name(name: &str) -> Option<Template> {
        match name {
            "trials" => Some(Template::trials()),
            "standard" => Some(Template::standard()),
            "dedicated" => Some(Template::dedicated()),
            _ => None,
        }
    }

    /// The pre-contract tier (old T3): every env shares the pool cluster; the
    /// RLS floor is load-bearing there. Policies still matter for identity,
    /// promotion order, and a later move to a dedicated template.
    pub fn trials() -> Template {
        Template {
            name: "trials",
            pooled: true,
            policies: EnvPolicy::defaults(),
        }
    }

    /// The standard paying tier (old T2): own clusters per recovery domain, with
    /// `canary` co-resident in `prod`'s domain (the T2 collapse).
    pub fn standard() -> Template {
        Template {
            name: "standard",
            pooled: false,
            policies: vec![
                EnvPolicy::dev(),
                canary(RecoveryDomain::SharedWith(Env::new("prod"))),
                EnvPolicy::prod(),
            ],
        }
    }

    /// The regulated tier (old T4): like `standard`, but `canary` owns its own
    /// recovery domain — a third cluster, maximal separation.
    pub fn dedicated() -> Template {
        Template {
            name: "dedicated",
            pooled: false,
            policies: vec![
                EnvPolicy::dev(),
                canary(RecoveryDomain::Own),
                EnvPolicy::prod(),
            ],
        }
    }

    /// The [`Placement`] this template stamps; a pooled template places on `pool`.
    pub fn placement(&self, pool: &str) -> Placement {
        if self.pooled {
            Placement::Pooled { pool: pool.into() }
        } else {
            Placement::Dedicated
        }
    }

    /// Stamp the template for an org: the [`Org`] (placement) plus its per-org
    /// policy rows — "a placement + an env-policy set in one step".
    pub fn stamp(&self, org_id: impl Into<String>, pool: &str) -> (Org, Vec<OrgEnvPolicy>) {
        let id: String = org_id.into();
        let rows = self
            .policies
            .iter()
            .cloned()
            .map(|policy| OrgEnvPolicy {
                org: id.clone(),
                policy,
            })
            .collect();
        (
            Org {
                id,
                placement: self.placement(pool),
            },
            rows,
        )
    }
}

/// The template `canary` policy: promotion rank between `dev` and `prod`, sized
/// like `prod` but lighter HA (2 instances — pre-prod traffic), with the given
/// recovery domain (`shared-with prod` = T2, `own` = T4 — the one-field
/// difference the templates encode).
fn canary(recovery_domain: RecoveryDomain) -> EnvPolicy {
    EnvPolicy {
        name: Env::new("canary"),
        recovery_domain,
        promotion_rank: 20,
        instances: 2,
        ..EnvPolicy::prod()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three presets reproduce the retired tiers: trials = pooled dev/prod;
    /// standard = dedicated with canary sharing prod's domain; dedicated =
    /// canary in its OWN domain (the one-field T2/T4 difference).
    #[test]
    fn templates_reproduce_the_retired_tiers() {
        let trials = Template::trials();
        assert!(trials.pooled);
        assert_eq!(
            trials.placement("wamn-pg"),
            Placement::Pooled {
                pool: "wamn-pg".into()
            }
        );
        let names: Vec<&str> = trials.policies.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["dev", "prod"]);

        let standard = Template::standard();
        assert!(!standard.pooled);
        assert_eq!(standard.placement("ignored"), Placement::Dedicated);
        let canary = standard
            .policies
            .iter()
            .find(|p| p.name == "canary")
            .expect("standard stamps a canary policy");
        assert_eq!(
            canary.recovery_domain,
            RecoveryDomain::SharedWith(Env::new("prod")),
            "standard canary co-resides in prod's recovery domain (T2)"
        );

        let dedicated = Template::dedicated();
        let canary = dedicated
            .policies
            .iter()
            .find(|p| p.name == "canary")
            .expect("dedicated stamps a canary policy");
        assert_eq!(
            canary.recovery_domain,
            RecoveryDomain::Own,
            "dedicated canary owns its recovery domain (T4)"
        );
        assert_eq!(canary.instances, 2, "canary is lighter HA than prod");
        assert_eq!(canary.promotion_rank, 20, "dev(10) < canary(20) < prod(30)");
    }

    /// Every shipped policy set is ordered by promotion rank and internally
    /// consistent (a shared-with target names a policy in the same set), so a
    /// stamped org validates without external context.
    #[test]
    fn template_policy_sets_are_self_consistent() {
        for name in Template::NAMES {
            let t = Template::by_name(name).expect("shipped template");
            assert_eq!(t.name, name);
            let ranks: Vec<i32> = t.policies.iter().map(|p| p.promotion_rank).collect();
            let mut sorted = ranks.clone();
            sorted.sort_unstable();
            assert_eq!(ranks, sorted, "{name}: policies ordered by promotion rank");
            for p in &t.policies {
                if let RecoveryDomain::SharedWith(target) = &p.recovery_domain {
                    assert!(
                        t.policies.iter().any(|q| &q.name == target),
                        "{name}: {:?} shares an unknown target {target:?}",
                        p.name
                    );
                }
            }
        }
        assert!(Template::by_name("platinum").is_none());
    }

    /// `stamp` produces the org placement + its per-org policy rows in one step.
    #[test]
    fn stamp_yields_the_placement_and_the_org_scoped_rows() {
        let (org, rows) = Template::standard().stamp("acme", "wamn-pg");
        assert_eq!(org.id, "acme");
        assert_eq!(org.placement, Placement::Dedicated);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.org == "acme"));

        let (org, rows) = Template::trials().stamp("trialco", "wamn-pg");
        assert_eq!(
            org.placement,
            Placement::Pooled {
                pool: "wamn-pg".into()
            }
        );
        assert_eq!(rows.len(), 2);
    }
}
