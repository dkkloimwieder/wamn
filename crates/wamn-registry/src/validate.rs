//! Structural validation of a [`Registry`].
//!
//! Checks well-formedness — id slug/reserved-prefix discipline, uniqueness, and
//! referential integrity (a project names a real org; a project-env names a real
//! project) — plus schema-format compatibility. It is pure and clock-free; the
//! live storage tables + their DB-enforced invariants land with `wamn-q3n.3`.

use std::collections::HashSet;

use crate::types::{Registry, SCHEMA_VERSION};

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

    // --- orgs: valid ids, unique, valid cluster refs ------------------------
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
        check_name(
            &mut issues,
            format!("orgs[{i}].prod-cluster.name"),
            &o.prod_cluster.name,
            "empty-cluster-name",
            "invalid-cluster-name",
        );
        check_name(
            &mut issues,
            format!("orgs[{i}].dev-cluster.name"),
            &o.dev_cluster.name,
            "empty-cluster-name",
            "invalid-cluster-name",
        );
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

    // --- project-envs: known project, unique triple, valid Secret ref -------
    let mut pe_keys: HashSet<(&str, &str, crate::types::Env)> = HashSet::new();
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
        if !pe_keys.insert((t.org.as_str(), t.project.as_str(), t.env)) {
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
        ClusterRef, Env, Org, Project, ProjectEnv, Registry, SecretRef, Tier, Triple,
    };

    fn org(id: &str, tier: Tier, prod: &str, dev: &str) -> Org {
        Org {
            id: id.into(),
            tier,
            prod_cluster: ClusterRef::new(prod),
            canary_cluster: None,
            dev_cluster: ClusterRef::new(dev),
        }
    }

    /// A minimal valid registry: one standard org, one project, one prod env.
    fn minimal() -> Registry {
        Registry {
            schema_version: "0.1".into(),
            orgs: vec![org("acme", Tier::Standard, "acme-prod", "acme-dev")],
            projects: vec![Project {
                org: "acme".into(),
                id: "billing".into(),
            }],
            project_envs: vec![ProjectEnv {
                triple: Triple::new("acme", "billing", Env::Prod),
                db_secret: SecretRef::new("wamn-db-billing"),
            }],
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
        // Org id under the reserved prefix.
        let mut r = minimal();
        r.orgs[0].id = "wamn-corp".into();
        // The project + project-env now reference the (renamed) org; keep them
        // pointed at it so the *only* new error is the reserved id.
        r.projects[0].org = "wamn-corp".into();
        r.project_envs[0].triple.org = "wamn-corp".into();
        assert!(codes(&r).contains(&"reserved-org-id"), "{:?}", r.issues());

        // Bare `wamn` is reserved too.
        let mut r = minimal();
        r.orgs[0].id = "wamn".into();
        r.projects[0].org = "wamn".into();
        r.project_envs[0].triple.org = "wamn".into();
        assert!(codes(&r).contains(&"reserved-org-id"));

        // Project id under the reserved prefix.
        let mut r = minimal();
        r.projects[0].id = "wamn-run".into();
        r.project_envs[0].triple.project = "wamn-run".into();
        assert!(codes(&r).contains(&"reserved-project-id"));

        // The boundary is a hyphen: `wamning` is a normal id.
        let mut r = minimal();
        r.orgs[0].id = "wamning".into();
        r.projects[0].org = "wamning".into();
        r.project_envs[0].triple.org = "wamning".into();
        assert!(r.is_valid(), "{:?}", r.issues());
    }

    #[test]
    fn non_slug_ids_are_invalid() {
        for bad in ["Acme", "under_score", "has.dot", "-lead", "trail-", "a b"] {
            let mut r = minimal();
            r.orgs[0].id = bad.into();
            r.projects[0].org = bad.into();
            r.project_envs[0].triple.org = bad.into();
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
    fn cluster_and_secret_names_may_carry_the_wamn_prefix() {
        // `wamn-pg` (the pool) and `wamn-db-*` (the Secret) are platform-minted
        // and must NOT trip the reserved-id rule.
        let mut r = minimal();
        r.orgs[0].tier = Tier::Trials;
        r.orgs[0].prod_cluster = ClusterRef::new("wamn-pg");
        r.orgs[0].dev_cluster = ClusterRef::new("wamn-pg");
        r.project_envs[0].db_secret = SecretRef::new("wamn-db-billing");
        assert!(r.is_valid(), "{:?}", r.issues());
    }

    #[test]
    fn empty_cluster_and_secret_names_are_errors() {
        let mut r = minimal();
        r.orgs[0].prod_cluster = ClusterRef::new("");
        r.project_envs[0].db_secret = SecretRef::new("");
        let c = codes(&r);
        assert!(c.contains(&"empty-cluster-name"));
        assert!(c.contains(&"empty-secret-name"));
    }

    #[test]
    fn duplicate_org_and_project_and_project_env_are_errors() {
        let mut r = minimal();
        r.orgs.push(org("acme", Tier::Trials, "wamn-pg", "wamn-pg"));
        assert!(codes(&r).contains(&"duplicate-org"));

        let mut r = minimal();
        r.projects.push(Project {
            org: "acme".into(),
            id: "billing".into(),
        });
        assert!(codes(&r).contains(&"duplicate-project"));

        let mut r = minimal();
        r.project_envs.push(ProjectEnv {
            triple: Triple::new("acme", "billing", Env::Prod),
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
    fn future_major_schema_version_is_unsupported() {
        let mut r = minimal();
        r.schema_version = "1.0".into();
        assert!(codes(&r).contains(&"unsupported-schema-version"));
    }
}
