//! Structural validation of a [`Registry`].
//!
//! Checks well-formedness — id slug/reserved-prefix discipline, uniqueness, and
//! referential integrity (a project names a real org; a project-env names a real
//! project **and** a real env policy) — plus schema-format compatibility and the
//! D18 env-policy integrity (`env` resolves to a policy; `shared-with` targets
//! exist and form no cycle). It is pure and clock-free; with the closed-enum
//! CHECK literals retired (`docs/deployment-model.md` §5, cjv.20), this is the
//! enforcement that holds on the in-memory `from_json` import path, not just at
//! DB insert.

use std::collections::{HashMap, HashSet};

use crate::types::{RecoveryDomain, Registry, SCHEMA_VERSION};

/// Severity of a validation [`Issue`]. Only [`Severity::Error`] makes a registry
/// invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A single validation finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    /// Stable machine code, e.g. `reserved-org-id`.
    pub code: &'static str,
    /// JSON-ish path to the offending element, e.g. `projects[2].org`.
    pub path: String,
    pub message: String,
}

impl Issue {
    fn error(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{sev} [{}] {}: {}", self.code, self.path, self.message)
    }
}

/// The platform-reserved id prefix (wamn-66x): the bare word `wamn` and any
/// `wamn-…` id are rejected for orgs/projects, since those ids mint
/// platform-owned cluster / Secret / schema names.
const RESERVED_PREFIX: &str = "wamn";

/// Max id length. Keeps a derived `wamn-db-<id>` Secret/database name within
/// Postgres's 63-byte identifier limit with margin (mirrors `wamn-provision`).
const MAX_ID_LEN: usize = 40;

/// Max K8s resource-name length (a DNS-1123 label).
const MAX_NAME_LEN: usize = 63;

fn is_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// A lowercase slug: `[a-z0-9-]`, starting and ending alphanumeric, non-empty.
/// The shared platform id discipline (`wamn-provision::validate_project_id`,
/// wi4 flow ids, 66x) — inlined to keep this foundational crate's dep closure
/// `{serde, serde_json}` and avoid a registry → provisioning coupling.
fn is_slug(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.iter().all(|&b| is_alnum(b) || b == b'-')
        && is_alnum(bytes[0])
        && is_alnum(bytes[bytes.len() - 1])
}

/// Whether `id` is under the reserved `wamn` prefix. The boundary is a hyphen,
/// so `wamning` is a normal id (mirrors the catalog 66x rule).
fn is_reserved(id: &str) -> bool {
    id == RESERVED_PREFIX || id.starts_with("wamn-")
}

/// Validate an org/project id: non-empty slug, within length, not reserved.
fn check_id(
    issues: &mut Vec<Issue>,
    path: String,
    id: &str,
    empty: &'static str,
    invalid: &'static str,
    reserved: &'static str,
) {
    if id.is_empty() {
        issues.push(Issue::error(empty, path, "id is required"));
    } else if id.len() > MAX_ID_LEN || !is_slug(id) {
        issues.push(Issue::error(
            invalid,
            path,
            format!(
                "id {id:?} must be a lowercase slug [a-z0-9-] (start/end alphanumeric, \
                 <= {MAX_ID_LEN} bytes) — it embeds into cluster/Secret/subdomain names"
            ),
        ));
    } else if is_reserved(id) {
        issues.push(Issue::error(
            reserved,
            path,
            format!("id {id:?} is under the reserved `wamn` prefix (wamn-66x)"),
        ));
    }
}

/// Validate an env slug: non-empty lowercase slug within length. Env slugs embed
/// into `<org>-<env>` cluster names and `<project>--<env>.<org>` subdomains but do
/// **not** mint platform-owned `wamn-*` names, so the reserved-prefix rule does
/// not apply (an env may be any well-formed slug).
fn check_env(
    issues: &mut Vec<Issue>,
    path: String,
    env: &str,
    empty: &'static str,
    invalid: &'static str,
) {
    if env.is_empty() {
        issues.push(Issue::error(empty, path, "env slug is required"));
    } else if env.len() > MAX_ID_LEN || !is_slug(env) {
        issues.push(Issue::error(
            invalid,
            path,
            format!("env {env:?} must be a lowercase slug [a-z0-9-] (<= {MAX_ID_LEN} bytes)"),
        ));
    }
}

/// Validate a cluster / Secret **name**. These are platform-minted DNS-1123
/// labels and *may* carry the `wamn` prefix (`wamn-pg`, `wamn-db-<project>`), so
/// only the charset/length is checked — the reserved-prefix rule does not apply.
fn check_name(
    issues: &mut Vec<Issue>,
    path: String,
    name: &str,
    empty: &'static str,
    invalid: &'static str,
) {
    if name.is_empty() {
        issues.push(Issue::error(empty, path, "name is required"));
    } else if name.len() > MAX_NAME_LEN || !is_slug(name) {
        issues.push(Issue::error(
            invalid,
            path,
            format!("name {name:?} must be a DNS-1123 label [a-z0-9-]"),
        ));
    }
}

/// Every issue (errors and warnings) for a registry, in a stable order.
pub fn validate(reg: &Registry) -> Vec<Issue> {
    let mut issues = Vec::new();

    // --- schema-format version ----------------------------------------------
    match compatible(&reg.schema_version) {
        Compat::Ok => {}
        Compat::Unparsable => issues.push(Issue::error(
            "bad-schema-version",
            "schema_version",
            format!("{:?} is not a MAJOR.MINOR version", reg.schema_version),
        )),
        Compat::Unsupported => issues.push(Issue::error(
            "unsupported-schema-version",
            "schema_version",
            format!(
                "{:?} is newer than this implementation ({SCHEMA_VERSION})",
                reg.schema_version
            ),
        )),
    }

    // --- orgs: valid ids, unique, valid placement ---------------------------
    let mut org_ids: HashSet<&str> = HashSet::new();
    for (i, o) in reg.orgs.iter().enumerate() {
        check_id(
            &mut issues,
            format!("orgs[{i}].id"),
            &o.id,
            "empty-org-id",
            "invalid-org-id",
            "reserved-org-id",
        );
        if !o.id.is_empty() && !org_ids.insert(o.id.as_str()) {
            issues.push(Issue::error(
                "duplicate-org",
                format!("orgs[{i}].id"),
                format!("org id {:?} is not unique", o.id),
            ));
        }
        // Placement: a pooled org's pool cluster must be a valid name; a dedicated
        // org's clusters are derived (`cluster_of`), so there is nothing to check.
        if let Some(pool) = o.placement.pool() {
            check_name(
                &mut issues,
                format!("orgs[{i}].placement.pool"),
                pool,
                "empty-cluster-name",
                "invalid-cluster-name",
            );
        }
    }

    // --- env policies (org-scoped, wamn-8df.4): valid slug names, unique per
    // org, known org, resolvable recovery domains within the org's own set ---
    let mut policy_keys: HashSet<(&str, &str)> = HashSet::new();
    for (i, p) in reg.env_policies.iter().enumerate() {
        check_env(
            &mut issues,
            format!("env-policies[{i}].policy.name"),
            &p.policy.name,
            "empty-env-policy-name",
            "invalid-env-policy-name",
        );
        if !org_ids.contains(p.org.as_str()) {
            issues.push(Issue::error(
                "unknown-org",
                format!("env-policies[{i}].org"),
                format!("env policy references unknown org {:?}", p.org),
            ));
        }
        if !p.policy.name.is_empty()
            && !policy_keys.insert((p.org.as_str(), p.policy.name.as_str()))
        {
            issues.push(Issue::error(
                "duplicate-env-policy",
                format!("env-policies[{i}].policy.name"),
                format!(
                    "env policy {:?} is not unique in org {:?}",
                    p.policy.name, p.org
                ),
            ));
        }
    }
    // A `shared-with` target must name a known policy IN THE SAME ORG's set
    // (referential integrity — the recovery-domain owner must exist for
    // `cluster_of` to derive a cluster).
    for (i, p) in reg.env_policies.iter().enumerate() {
        if let RecoveryDomain::SharedWith(target) = &p.policy.recovery_domain
            && !policy_keys.contains(&(p.org.as_str(), target.as_str()))
        {
            issues.push(Issue::error(
                "unknown-shared-with-target",
                format!("env-policies[{i}].policy.recovery-domain"),
                format!(
                    "env policy {:?} (org {:?}) shares the recovery domain of {:?}, \
                     which is not one of that org's policies",
                    p.policy.name, p.org, target
                ),
            ));
        }
    }
    // No `shared-with` cycle within an org (a functional graph per org — each
    // policy points at ≤1 target). A cycle has no `own` root, so `cluster_of`
    // would not terminate.
    let target_of: HashMap<(&str, &str), &str> = reg
        .env_policies
        .iter()
        .filter_map(|p| match &p.policy.recovery_domain {
            RecoveryDomain::SharedWith(t)
                if policy_keys.contains(&(p.org.as_str(), t.as_str())) =>
            {
                Some(((p.org.as_str(), p.policy.name.as_str()), t.as_str()))
            }
            _ => None,
        })
        .collect();
    let mut cycle_members: HashSet<(&str, &str)> = HashSet::new();
    for (i, p) in reg.env_policies.iter().enumerate() {
        let key = (p.org.as_str(), p.policy.name.as_str());
        if cycle_members.contains(&key) {
            continue;
        }
        let mut path: Vec<&str> = vec![p.policy.name.as_str()];
        let mut cur = p.policy.name.as_str();
        while let Some(&next) = target_of.get(&(p.org.as_str(), cur)) {
            if let Some(pos) = path.iter().position(|&n| n == next) {
                for &m in &path[pos..] {
                    cycle_members.insert((p.org.as_str(), m));
                }
                issues.push(Issue::error(
                    "shared-with-cycle",
                    format!("env-policies[{i}].policy.recovery-domain"),
                    format!(
                        "env policy {:?} (org {:?}) is in a recovery-domain shared-with cycle",
                        p.policy.name, p.org
                    ),
                ));
                break;
            }
            path.push(next);
            cur = next;
        }
    }

    // --- projects: valid ids, known org, unique per org ---------------------
    let mut project_keys: HashSet<(&str, &str)> = HashSet::new();
    for (i, p) in reg.projects.iter().enumerate() {
        check_id(
            &mut issues,
            format!("projects[{i}].id"),
            &p.id,
            "empty-project-id",
            "invalid-project-id",
            "reserved-project-id",
        );
        if !org_ids.contains(p.org.as_str()) {
            issues.push(Issue::error(
                "unknown-org",
                format!("projects[{i}].org"),
                format!("project references unknown org {:?}", p.org),
            ));
        }
        if !p.id.is_empty() && !project_keys.insert((p.org.as_str(), p.id.as_str())) {
            issues.push(Issue::error(
                "duplicate-project",
                format!("projects[{i}]"),
                format!("project {:?} is not unique in org {:?}", p.id, p.org),
            ));
        }
    }

    // --- project-envs: known project + env policy, unique triple, valid Secret ---
    let mut pe_keys: HashSet<(&str, &str, &str)> = HashSet::new();
    for (i, pe) in reg.project_envs.iter().enumerate() {
        let t = &pe.triple;
        if !project_keys.contains(&(t.org.as_str(), t.project.as_str())) {
            issues.push(Issue::error(
                "unknown-project",
                format!("project-envs[{i}].triple"),
                format!(
                    "project-env references unknown project {:?} in org {:?}",
                    t.project, t.org
                ),
            ));
        }
        // The env slug must be well-formed AND name a policy in ITS ORG's set
        // (D18 + 8df.4: validity is "well-formed slug + resolves the org's
        // policy", replacing the closed CHECK).
        check_env(
            &mut issues,
            format!("project-envs[{i}].triple.env"),
            &t.env,
            "empty-env",
            "invalid-env",
        );
        if is_slug(&t.env) && !policy_keys.contains(&(t.org.as_str(), t.env.as_str())) {
            issues.push(Issue::error(
                "unknown-env",
                format!("project-envs[{i}].triple.env"),
                format!("env {:?} names no env policy in org {:?}", t.env, t.org),
            ));
        }
        if !pe_keys.insert((t.org.as_str(), t.project.as_str(), t.env.as_str())) {
            issues.push(Issue::error(
                "duplicate-project-env",
                format!("project-envs[{i}].triple"),
                format!(
                    "(org={:?}, project={:?}, env={}) is not unique",
                    t.org, t.project, t.env
                ),
            ));
        }
        check_name(
            &mut issues,
            format!("project-envs[{i}].db-secret.name"),
            &pe.db_secret.name,
            "empty-secret-name",
            "invalid-secret-name",
        );
    }

    issues
}

enum Compat {
    Ok,
    Unparsable,
    Unsupported,
}

/// A registry's `schema_version` is compatible if its MAJOR matches and its
/// MINOR is not newer than what this crate implements (additive-within-major,
/// per the `0.1.x` freeze rule — mirrors `wamn-flow`).
fn compatible(v: &str) -> Compat {
    let parse = |s: &str| -> Option<(u32, u32)> {
        let (maj, min) = s.split_once('.')?;
        Some((maj.parse().ok()?, min.parse().ok()?))
    };
    let (Some((maj, min)), Some((smaj, smin))) = (parse(v), parse(SCHEMA_VERSION)) else {
        return Compat::Unparsable;
    };
    if maj != smaj || min > smin {
        Compat::Unsupported
    } else {
        Compat::Ok
    }
}

impl Registry {
    /// All validation issues (errors and warnings).
    pub fn issues(&self) -> Vec<Issue> {
        validate(self)
    }

    /// `true` if the registry has no error-severity issues.
    pub fn is_valid(&self) -> bool {
        !validate(self).iter().any(|i| i.severity == Severity::Error)
    }

    /// `Ok` if valid, else the error-severity issues.
    pub fn validate(&self) -> Result<(), Vec<Issue>> {
        let errors: Vec<Issue> = validate(self)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{
        Env, EnvPolicy, Org, OrgEnvPolicy, Project, ProjectEnv, RecoveryDomain, Registry,
        SecretRef, Triple,
    };

    fn policies_for(org: &str, policies: Vec<EnvPolicy>) -> Vec<OrgEnvPolicy> {
        policies
            .into_iter()
            .map(|policy| OrgEnvPolicy {
                org: org.into(),
                policy,
            })
            .collect()
    }

    /// A minimal valid registry: one dedicated org with its stamped default
    /// policies, one pooled org, a project, and a prod env.
    fn minimal() -> Registry {
        Registry {
            schema_version: "0.1".into(),
            env_policies: policies_for("acme", EnvPolicy::defaults()),
            orgs: vec![Org::dedicated("acme"), Org::pooled("try", "wamn-pg")],
            projects: vec![Project {
                org: "acme".into(),
                id: "billing".into(),
            }],
            project_envs: vec![ProjectEnv {
                triple: Triple::new("acme", "billing", "prod"),
                db_secret: SecretRef::new("wamn-db-billing"),
            }],
        }
    }

    /// Rename the first org across every row that references it (orgs, projects,
    /// project-envs, and its policy rows).
    fn rename_first_org(r: &mut Registry, id: &str) {
        r.orgs[0].id = id.into();
        r.projects[0].org = id.into();
        r.project_envs[0].triple.org = id.into();
        for p in &mut r.env_policies {
            if p.org == "acme" {
                p.org = id.into();
            }
        }
    }

    fn codes(reg: &Registry) -> Vec<&'static str> {
        reg.issues().into_iter().map(|i| i.code).collect()
    }

    #[test]
    fn minimal_registry_is_valid() {
        let r = minimal();
        assert!(r.is_valid(), "issues: {:?}", r.issues());
        assert!(r.validate().is_ok());
    }

    #[test]
    fn empty_registry_is_valid() {
        assert!(Registry::empty().is_valid());
    }

    #[test]
    fn reserved_wamn_prefix_is_rejected_on_org_and_project_ids() {
        let mut r = minimal();
        rename_first_org(&mut r, "wamn-corp");
        assert!(codes(&r).contains(&"reserved-org-id"), "{:?}", r.issues());

        // Bare `wamn` is reserved too.
        let mut r = minimal();
        rename_first_org(&mut r, "wamn");
        assert!(codes(&r).contains(&"reserved-org-id"));

        // Project id under the reserved prefix.
        let mut r = minimal();
        r.projects[0].id = "wamn-run".into();
        r.project_envs[0].triple.project = "wamn-run".into();
        assert!(codes(&r).contains(&"reserved-project-id"));

        // The boundary is a hyphen: `wamning` is a normal id.
        let mut r = minimal();
        rename_first_org(&mut r, "wamning");
        assert!(r.is_valid(), "{:?}", r.issues());
    }

    #[test]
    fn non_slug_ids_are_invalid() {
        for bad in ["Acme", "under_score", "has.dot", "-lead", "trail-", "a b"] {
            let mut r = minimal();
            rename_first_org(&mut r, bad);
            assert!(
                codes(&r).contains(&"invalid-org-id"),
                "{bad:?} should be invalid"
            );
        }
        // Empty is its own, earlier code (charset check skipped).
        let mut r = minimal();
        r.orgs[0].id = "".into();
        let c = codes(&r);
        assert!(c.contains(&"empty-org-id"));
        assert!(!c.contains(&"invalid-org-id"));
    }

    #[test]
    fn pool_cluster_name_may_carry_the_wamn_prefix_but_must_be_valid() {
        // `wamn-pg` (the pool) is platform-minted and must NOT trip the reserved
        // rule; an empty / non-slug pool name is an error.
        let mut r = minimal();
        r.orgs[1] = Org::pooled("try", "wamn-pg");
        assert!(r.is_valid(), "{:?}", r.issues());

        let mut r = minimal();
        r.orgs[1] = Org::pooled("try", "");
        assert!(codes(&r).contains(&"empty-cluster-name"));

        let mut r = minimal();
        r.orgs[1] = Org::pooled("try", "Bad_Pool");
        assert!(codes(&r).contains(&"invalid-cluster-name"));
    }

    #[test]
    fn duplicate_org_project_and_project_env_are_errors() {
        let mut r = minimal();
        r.orgs.push(Org::dedicated("acme"));
        assert!(codes(&r).contains(&"duplicate-org"));

        let mut r = minimal();
        r.projects.push(Project {
            org: "acme".into(),
            id: "billing".into(),
        });
        assert!(codes(&r).contains(&"duplicate-project"));

        let mut r = minimal();
        r.project_envs.push(ProjectEnv {
            triple: Triple::new("acme", "billing", "prod"),
            db_secret: SecretRef::new("wamn-db-billing"),
        });
        assert!(codes(&r).contains(&"duplicate-project-env"));
    }

    #[test]
    fn referential_integrity_is_enforced() {
        // Project names an org that isn't registered.
        let mut r = minimal();
        r.projects[0].org = "ghost".into();
        r.project_envs[0].triple.org = "ghost".into();
        assert!(codes(&r).contains(&"unknown-org"));

        // Project-env names a project that isn't registered.
        let mut r = minimal();
        r.project_envs[0].triple.project = "ghost".into();
        assert!(codes(&r).contains(&"unknown-project"));
    }

    #[test]
    fn project_env_env_must_resolve_to_a_policy() {
        // A well-formed env slug that names no policy is an error (D18: env is
        // valid iff it resolves a policy — the CHECK's replacement).
        let mut r = minimal();
        r.project_envs[0].triple.env = Env::new("staging");
        assert!(codes(&r).contains(&"unknown-env"), "{:?}", r.issues());

        // Adding the policy TO THE ORG's set makes it valid.
        let mut r = minimal();
        r.project_envs[0].triple.env = Env::new("staging");
        r.env_policies.push(OrgEnvPolicy {
            org: "acme".into(),
            policy: EnvPolicy {
                name: Env::new("staging"),
                ..EnvPolicy::dev()
            },
        });
        assert!(r.is_valid(), "{:?}", r.issues());

        // A malformed env slug is invalid-env (and not double-reported as unknown).
        let mut r = minimal();
        r.project_envs[0].triple.env = Env::new("Prod");
        let c = codes(&r);
        assert!(c.contains(&"invalid-env"));
        assert!(!c.contains(&"unknown-env"));
    }

    #[test]
    fn policies_are_org_scoped_not_shared_across_orgs() {
        // ANOTHER org's policy row does not satisfy this org's project-env: a
        // 'staging' policy stamped only for 'try' leaves acme's staging unknown
        // (8df.4 — each org owns its policy set).
        let mut r = minimal();
        r.project_envs[0].triple.env = Env::new("staging");
        r.env_policies.push(OrgEnvPolicy {
            org: "try".into(),
            policy: EnvPolicy {
                name: Env::new("staging"),
                ..EnvPolicy::dev()
            },
        });
        assert!(codes(&r).contains(&"unknown-env"), "{:?}", r.issues());

        // A policy row under an unregistered org is a referential error.
        let mut r = minimal();
        r.env_policies.push(OrgEnvPolicy {
            org: "ghost".into(),
            policy: EnvPolicy::dev(),
        });
        assert!(codes(&r).contains(&"unknown-org"), "{:?}", r.issues());
    }

    #[test]
    fn canary_shared_with_prod_is_valid() {
        // The shipped T2 canary: a policy sharing prod's recovery domain, added as
        // data with no enum variant.
        let mut r = minimal();
        r.env_policies.push(OrgEnvPolicy {
            org: "acme".into(),
            policy: EnvPolicy {
                name: Env::new("canary"),
                recovery_domain: RecoveryDomain::SharedWith(Env::new("prod")),
                promotion_rank: 20,
                ..EnvPolicy::prod()
            },
        });
        r.project_envs.push(ProjectEnv {
            triple: Triple::new("acme", "billing", "canary"),
            db_secret: SecretRef::new("wamn-db-billing-canary"),
        });
        assert!(r.is_valid(), "{:?}", r.issues());
    }

    #[test]
    fn env_policy_integrity_is_enforced() {
        // Duplicate policy name within one org.
        let mut r = minimal();
        r.env_policies.push(OrgEnvPolicy {
            org: "acme".into(),
            policy: EnvPolicy::prod(),
        });
        assert!(codes(&r).contains(&"duplicate-env-policy"));

        // The SAME policy name in a DIFFERENT org is fine (org-scoped keying).
        let mut r = minimal();
        r.env_policies.push(OrgEnvPolicy {
            org: "try".into(),
            policy: EnvPolicy::prod(),
        });
        assert!(r.is_valid(), "{:?}", r.issues());

        // shared-with names a policy the org doesn't have.
        let mut r = minimal();
        r.env_policies.push(OrgEnvPolicy {
            org: "acme".into(),
            policy: EnvPolicy {
                name: Env::new("canary"),
                recovery_domain: RecoveryDomain::SharedWith(Env::new("ghost")),
                ..EnvPolicy::dev()
            },
        });
        assert!(codes(&r).contains(&"unknown-shared-with-target"));

        // A shared-with cycle (a <-> b) within an org has no `own` root.
        let mut r = minimal();
        r.env_policies = policies_for(
            "acme",
            vec![
                EnvPolicy {
                    name: Env::new("a"),
                    recovery_domain: RecoveryDomain::SharedWith(Env::new("b")),
                    ..EnvPolicy::dev()
                },
                EnvPolicy {
                    name: Env::new("b"),
                    recovery_domain: RecoveryDomain::SharedWith(Env::new("a")),
                    ..EnvPolicy::dev()
                },
            ],
        );
        // (drop the project-env whose 'prod' policy no longer exists)
        r.project_envs.clear();
        assert!(codes(&r).contains(&"shared-with-cycle"), "{:?}", r.issues());
    }

    #[test]
    fn future_major_schema_version_is_unsupported() {
        let mut r = minimal();
        r.schema_version = "1.0".into();
        assert!(codes(&r).contains(&"unsupported-schema-version"));
    }
}
