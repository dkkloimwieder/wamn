//! Identifier naming + the connection-URL composer.
//!
//! A project id is a K8s-friendly lowercase slug (`[a-z0-9-]`, start/end
//! alphanumeric) — the same shape the platform uses for flow ids (wi4). It maps
//! to a database and Secret named `wamn-db-<project>`. Hyphenated names are
//! quoted in DDL (see [`crate::sql`]) and are unreserved in a connection URL
//! path, so one slug serves both the K8s (hyphen) and Postgres (quoted) domains
//! without translation.

use crate::error::ProvisionError;

/// The single shared, cluster-global application role. Every generated tenant
/// floor and hand-written schema grants to it; provisioning grants it `CONNECT`
/// on each project database (isolation is per-database + RLS, not per-role — see
/// the crate docs).
pub const APP_ROLE: &str = "wamn_app";

/// The role every project-env database's `datdba` points at — the title holder,
/// and nothing else (wamn-0h0g R9).
///
/// **Owner-only by construction**: `NOLOGIN`, zero grants, zero memberships, and
/// no credential-generation pair. Nothing authenticates as it, so there is
/// nothing to rotate. The rights database ownership implies — `ALTER DATABASE`,
/// `DROP DATABASE`, `CREATE SCHEMA`, `GRANT … ON DATABASE`, `TEMPORARY` — are
/// exercised only by the superuser provisioning principal that applies the
/// `Database` CR and the privilege SQL.
///
/// Deliberately **not** [`APP_ROLE`]: guest-authored SQL executes as `wamn_app`,
/// and ownership confers authority no grant matrix expresses and no `REVOKE`
/// can remove (`NOCREATEDB` restrains none of it on an already-owned database).
/// Deliberately **not** `postgres` either: a superuser-owned tenant database
/// trades a guest-reachable owner for a superuser one. Every other runtime role
/// in this repository is probed with "owns the current database" treated as
/// DISQUALIFYING; this role is the one principal for which ownership is the
/// whole point.
pub const DB_OWNER_ROLE: &str = "wamn_db_owner";

/// Prefix for the fixed-mount effect-writer credential Secret.
pub const EFFECT_WRITER_SECRET_PREFIX: &str = "wamn-effect-writer-";

/// Prefix for the per-project database **and** Secret name: `wamn-db-<project>`.
/// It is under the platform-reserved `wamn` prefix (wamn-66x) on purpose — the
/// platform mints it, and project ids in that space are rejected.
pub const DB_PREFIX: &str = "wamn-db-";

/// Max project-id length. Keeps `wamn-db-<project>` within Postgres's 63-byte
/// identifier limit (`63 - len("wamn-db-") = 55`) with comfortable margin.
pub const MAX_PROJECT_ID_LEN: usize = 40;

/// Max length (bytes) of a provisioned database / K8s resource name: Postgres's
/// identifier limit (63) — also within the DNS-1123 **label** limit (63) that the
/// CNPG `Database` resource name must satisfy. Per-project-env names encode the
/// full triple, so the assembled length is validated (see [`validate_project_env`]).
pub const MAX_DB_NAME_LEN: usize = 63;

/// Prefix for the per-project-env Kubernetes **namespace**:
/// `wamn-<org>--<project>--<env>--<instance>` (wamn-0h0g.15.3). Under the
/// platform-reserved `wamn` prefix like every other minted name, and carrying the
/// same `--` separated triple as `wamn-db-<org>--<project>--<env>`.
pub const NAMESPACE_PREFIX: &str = "wamn-";

/// Max length (bytes) of the per-project-env namespace: the DNS-1123 **label**
/// limit a Kubernetes namespace name must satisfy. The triple is NEVER truncated
/// or hash-suffixed to fit — a breaching triple is refused at provisioning
/// (see [`validate_project_env`]).
pub const MAX_NAMESPACE_LEN: usize = 63;

/// Length (bytes) of the provision-minted **instance suffix** every per-project-env
/// namespace carries: 8 characters over `[a-z0-9]` — the form settled in
/// `docs/archive/PLAN/PLAN.md`, ~41 bits.
///
/// It exists for REUSE AFTER DELETION: delete an environment and recreate it
/// under the same triple, and without a fresh suffix the new one inherits the old
/// namespace and whatever survived inside it. Randomness is the whole uniqueness
/// mechanism — no naming registry, no retry loop, no derivation (wamn-0h0g.13.57).
/// The mint belongs to the effect driver (`wamn-ctl provision-project-env`, which
/// stores it on `registry.project_envs`); this crate is pure and only derives.
pub const INSTANCE_SUFFIX_LEN: usize = 8;

/// Separator between the identity triple and the instance suffix — the same `--`
/// the triple's own components use. Components admit no `--` run and the suffix
/// admits no hyphen at all, so the assembled namespace stays an unambiguous
/// `--`-joined sequence.
const INSTANCE_SUFFIX_SEPARATOR: &str = "--";

/// Max length (bytes) of the namespace's identity **stem**
/// `wamn-<org>--<project>--<env>`: [`MAX_NAMESPACE_LEN`] less the separator and
/// the [`INSTANCE_SUFFIX_LEN`] suffix every provisioned namespace carries. This —
/// not [`MAX_NAMESPACE_LEN`] — is what a triple is refused against, because the
/// triple never gets the whole label.
pub const MAX_NAMESPACE_STEM_LEN: usize =
    MAX_NAMESPACE_LEN - INSTANCE_SUFFIX_SEPARATOR.len() - INSTANCE_SUFFIX_LEN;

/// Prefix for the per-project-env CDC **credential Secret**:
/// `wamn-cdc-<org>--<project>--<env>` (wamn-l5i9.9, D19 v3) — the hyphenated K8s
/// convention, a sibling of the `wamn-db-…` query-credential Secret and the
/// registration's `replication_secret_name` reference.
pub const CDC_SECRET_PREFIX: &str = "wamn-cdc-";

/// Prefix for the per-project-env CDC **Postgres objects** — the publication,
/// the failover replication slot, and the replication role share one name:
/// `wamn_cdc_<org>__<project>__<env>` with slug hyphens mapped to `_`.
/// Underscored because a replication **slot** name admits only `[a-z0-9_]`
/// (slots are not identifiers and cannot be quoted); the publication and role
/// reuse it so the whole CDC surface carries one name.
pub const CDC_OBJECT_PREFIX: &str = "wamn_cdc_";

/// The shared slug discipline for a provisioned-name identity component: a
/// non-empty lowercase slug `[a-z0-9-]`, starting and ending alphanumeric, with
/// **no run of consecutive hyphens**, at most [`MAX_PROJECT_ID_LEN`] bytes.
/// Returns the rejection reason, or `None` when well-formed.
///
/// The consecutive-hyphen ban (wamn-R27) closes an identity-collision: `--` is
/// the component separator in [`project_env_database_name`] and (mapped to `__`)
/// in [`cdc_object_name`], so a `--` run *inside* a component would let two
/// distinct triples — e.g. `(a, x--p, dev)` and `(a--x, p, dev)` — derive the
/// SAME database and CDC role/slot/publication names. `_` is not a second vector:
/// the charset ([`is_slug_byte`]) admits no underscore, so a `-`→`_` mapping only
/// ever yields an isolated `_` and the `__` separator stays unambiguous.
fn slug_reason(id: &str) -> Option<&'static str> {
    if id.is_empty() {
        return Some("empty");
    }
    if id.len() > MAX_PROJECT_ID_LEN {
        return Some("too long (max 40 bytes)");
    }
    if !id.bytes().all(is_slug_byte) {
        return Some("only lowercase letters, digits, and hyphens are allowed");
    }
    let bytes = id.as_bytes();
    if !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
        return Some("must start and end with a lowercase letter or digit");
    }
    if id.contains("--") {
        return Some("must not contain consecutive hyphens");
    }
    None
}

/// Validate a project id: a non-empty lowercase slug `[a-z0-9-]`, starting and
/// ending alphanumeric, with no consecutive hyphens, at most
/// [`MAX_PROJECT_ID_LEN`] bytes, and not under the reserved `wamn` prefix.
///
/// Lowercase + hyphen (not underscore) is deliberate: the id is both a K8s
/// Secret-name suffix (hyphens, no underscores) and — quoted — a database name.
pub fn validate_project_id(id: &str) -> Result<(), ProvisionError> {
    if let Some(reason) = slug_reason(id) {
        return Err(ProvisionError::InvalidProjectId {
            id: id.to_string(),
            reason,
        });
    }
    // wamn-66x: the `wamn` prefix is platform-reserved. The id is already
    // lowercase; reject the bare word and any `wamn-…` id (the boundary is a
    // hyphen, so `wamning` is fine — mirrors the catalog reserved-prefix rule).
    if id == "wamn" || id.starts_with("wamn-") {
        return Err(ProvisionError::ReservedProjectId { id: id.to_string() });
    }
    Ok(())
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

fn is_slug_byte(b: u8) -> bool {
    is_alnum(b) || b == b'-'
}

/// The project's database name: `wamn-db-<project>`. Quote it in DDL (it
/// contains hyphens) via [`crate::sql`]; it is URL-path-safe as-is.
pub fn database_name(project: &str) -> String {
    format!("{DB_PREFIX}{project}")
}

/// The project's credential Secret name: `wamn-db-<project>` — the same string
/// the future `K8sSecretProvider` (5x0.1) will look up.
pub fn secret_name(project: &str) -> String {
    format!("{DB_PREFIX}{project}")
}

/// The per-project-env database name: `wamn-db-<org>--<project>--<env>`
/// (wamn-q3n.7).
///
/// The **org** is encoded — unlike the 2.3 [`database_name`] — because the shared
/// T3 trials pool hosts many orgs (two orgs' identically-named projects would
/// otherwise collide on one cluster), and because every cluster's CNPG `Database`
/// resources share the one K8s namespace, so the resource name must be unique
/// there. `--` separates the identity components (the `Triple::host_label`
/// convention). Validate the assembled length with [`validate_project_env`]
/// before use; quote it in DDL (it contains hyphens) via [`crate::sql`].
pub fn project_env_database_name(org: &str, project: &str, env: &str) -> String {
    format!("{DB_PREFIX}{org}--{project}--{env}")
}

/// The per-project-env credential Secret name — identical to the database name
/// (`wamn-db-<org>--<project>--<env>`), the single lookup key the future
/// `K8sSecretProvider` (5x0.1) reads and the registry records as the project-env's
/// `SecretRef`.
pub fn project_env_secret_name(org: &str, project: &str, env: &str) -> String {
    project_env_database_name(org, project, env)
}

/// The per-project-env Kubernetes namespace:
/// `wamn-<org>--<project>--<env>--<instance>`.
///
/// One wamn environment `(org, project, env)` is one namespace (wamn-0h0g.15.3),
/// so every environment-scoped resource — the credential Secrets, the release
/// manifest, the environment's hosts — is addressed by DERIVING this name from
/// the triple and the environment's minted instance suffix. It is never
/// configured alongside the triple and never parsed back out of a resource.
///
/// `instance` is the environment's stored `registry.project_envs.instance_suffix`,
/// NOT a fresh draw: minting is the effect driver's job, and re-deriving under a
/// new suffix would orphan every resource the old one named. The suffix is what
/// makes delete-and-recreate safe — the recreated environment lands in a
/// different namespace and cannot inherit the deleted one's contents.
///
/// Validate the triple's assembled length with [`validate_project_env`] and the
/// suffix with [`validate_instance_suffix`] before use; a triple that does not
/// fit is refused, not shortened.
pub fn project_env_namespace(org: &str, project: &str, env: &str, instance: &str) -> String {
    format!(
        "{}{INSTANCE_SUFFIX_SEPARATOR}{instance}",
        project_env_namespace_stem(org, project, env)
    )
}

/// The namespace's identity stem `wamn-<org>--<project>--<env>` — the part the
/// triple contributes, bounded by [`MAX_NAMESPACE_STEM_LEN`].
fn project_env_namespace_stem(org: &str, project: &str, env: &str) -> String {
    format!("{NAMESPACE_PREFIX}{org}--{project}--{env}")
}

/// Validate a minted instance suffix: exactly [`INSTANCE_SUFFIX_LEN`] bytes over
/// `[a-z0-9]`.
///
/// The charset is narrower than a component slug's on purpose. The suffix is
/// APPENDED to a DNS-1123 label, so it decides that label's last byte and a
/// DNS-1123 label must end alphanumeric; admitting `-` would let a stored suffix
/// mint a namespace Kubernetes refuses. Call it on the value read back from
/// `registry.project_envs` — the registry is a trust boundary, not the minter.
pub fn validate_instance_suffix(instance: &str) -> Result<(), ProvisionError> {
    let reason = if instance.len() != INSTANCE_SUFFIX_LEN {
        "must be exactly 8 bytes"
    } else if !instance.bytes().all(is_alnum) {
        "only lowercase letters and digits are allowed"
    } else {
        return Ok(());
    };
    Err(ProvisionError::InvalidComponent {
        component: "instance",
        value: instance.to_string(),
        reason,
    })
}

/// The fixed-mount effect-writer credential Secret name.
pub fn project_env_effect_writer_secret_name(org: &str, project: &str, env: &str) -> String {
    format!("{EFFECT_WRITER_SECRET_PREFIX}{org}--{project}--{env}")
}

/// Validate that a `(org, project, env)` yields safe provisioned names: **all
/// three** identity components are slugs, the assembled database / Secret name
/// fits [`MAX_DB_NAME_LEN`] — a legal Postgres identifier and a legal DNS-1123
/// label for the CNPG `Database` resource — and the namespace's identity stem
/// fits [`MAX_NAMESPACE_STEM_LEN`], the budget left once the instance suffix
/// takes its share of the DNS-1123 label.
///
/// Both assembled names are bounded here, and neither is ever truncated or
/// hash-suffixed to fit: a triple that does not fit is REFUSED, at the one point
/// that can mint it. The namespace carries its own bound rather than inheriting
/// the database one — the two are not related by a fixed offset, and the
/// namespace-per-environment mapping must not rest on a coincidence.
///
/// ORDER IS LOAD-BEARING, and it is the LOOSER bound that goes first: check the
/// tighter one first and the other branch is dead code, so its bound is never
/// enforced in its own right (wamn-0h0g.15.72). The instance suffix INVERTED
/// which bound is looser — it spends 10 of the namespace's 63 bytes, leaving the
/// triple 53 there against 51 in the database name — so the database name is now
/// checked FIRST and the namespace second. Both branches stay reachable: a triple
/// of 52 or more combined bytes trips the database bound, one of 45 to 51 trips
/// the namespace bound.
///
/// Every component is slug-checked (wamn-R27): the org and env — not only the
/// project — separate on `--`/`__` into the derived database and CDC object
/// names, so a malformed component (a consecutive-hyphen run above all) is an
/// identity-collision vector, not merely the registry's concern. The project
/// carries the extra reserved-`wamn` rule ([`validate_project_id`]); org and env
/// are not reserved (an env may be any slug; the org's reserved-prefix rule is
/// the registry's at org creation), so they get the plain slug discipline.
pub fn validate_project_env(org: &str, project: &str, env: &str) -> Result<(), ProvisionError> {
    if let Some(reason) = slug_reason(org) {
        return Err(ProvisionError::InvalidComponent {
            component: "org",
            value: org.to_string(),
            reason,
        });
    }
    validate_project_id(project)?;
    if let Some(reason) = slug_reason(env) {
        return Err(ProvisionError::InvalidComponent {
            component: "env",
            value: env.to_string(),
            reason,
        });
    }
    let name = project_env_database_name(org, project, env);
    if name.len() > MAX_DB_NAME_LEN {
        return Err(ProvisionError::NameTooLong {
            name,
            max: MAX_DB_NAME_LEN,
        });
    }
    let stem = project_env_namespace_stem(org, project, env);
    if stem.len() > MAX_NAMESPACE_STEM_LEN {
        return Err(ProvisionError::NameTooLong {
            name: stem,
            max: MAX_NAMESPACE_STEM_LEN,
        });
    }
    Ok(())
}

/// The per-project-env CDC credential Secret name:
/// `wamn-cdc-<org>--<project>--<env>` (wamn-l5i9.9). Distinct from the
/// `wamn-db-…` query-credential Secret — the replication credential is a
/// separate, higher-privilege tier (R8b), so a leaked query credential never
/// implies the WAL and vice versa.
pub fn project_env_cdc_secret_name(org: &str, project: &str, env: &str) -> String {
    format!("{CDC_SECRET_PREFIX}{org}--{project}--{env}")
}

/// The shared name of the per-project-env CDC Postgres objects — publication,
/// failover replication slot, and replication role:
/// `wamn_cdc_<org>__<project>__<env>`, slug hyphens mapped to `_` and `__` as
/// the separator (a slot name admits only `[a-z0-9_]`). Since slugs cannot
/// contain `_`, the mapping keeps distinct triples distinct except for the same
/// consecutive-hyphen ambiguity the `--` database-name separator already
/// carries; identity always travels in the registration row / Secret labels,
/// never parsed back out of a name. Validate the assembled length with
/// [`validate_project_env_cdc`] before use.
pub fn cdc_object_name(org: &str, project: &str, env: &str) -> String {
    let flat = |s: &str| s.replace('-', "_");
    format!(
        "{CDC_OBJECT_PREFIX}{}__{}__{}",
        flat(org),
        flat(project),
        flat(env)
    )
}

/// The JetStream stream a project-env's CDC envelopes land in:
/// `EVT_<org>_<env>` (D19 v3 §5; the streambench-proven contract). Recorded in
/// the reader registration — the row is the source the reader publishes by, so
/// a policy refinement (e.g. a shared trials stream) is a data change, not a
/// rename.
pub fn event_stream_name(org: &str, env: &str) -> String {
    format!("EVT_{org}_{env}")
}

/// Validate that a `(org, project, env)` yields safe CDC names: the base
/// project-env validation ([`validate_project_env`]) plus the assembled
/// `wamn_cdc_…` object name — one byte per component longer than the database
/// name — fitting Postgres's 63-byte limit (a slot/publication/role name; the
/// like-sized `wamn-cdc-…` Secret name is comfortably within the K8s bound).
///
/// The extra bound is currently SUBSUMED: it admits a 50-byte triple where the
/// base validation's namespace stem admits only 44, so the base always refuses
/// first. It stays because it bounds a name the base does not, and it binds again
/// the moment the CDC objects carry the instance suffix — they are cluster-global
/// Postgres names, so they need it as much as the namespace does.
pub fn validate_project_env_cdc(org: &str, project: &str, env: &str) -> Result<(), ProvisionError> {
    validate_project_env(org, project, env)?;
    let name = cdc_object_name(org, project, env);
    if name.len() > MAX_DB_NAME_LEN {
        return Err(ProvisionError::NameTooLong {
            name,
            max: MAX_DB_NAME_LEN,
        });
    }
    Ok(())
}

/// Compose a libpq connection URL. Userinfo and the database path segment are
/// percent-encoded, so a password with URL-reserved characters is carried
/// safely (tokio_postgres percent-decodes them).
pub fn compose_url(user: &str, password: &str, host: &str, port: u16, database: &str) -> String {
    format!(
        "postgres://{}:{}@{}:{}/{}",
        pct(user),
        pct(password),
        host,
        port,
        pct(database),
    )
}

/// Percent-encode everything outside the URL unreserved set `[A-Za-z0-9-._~]`.
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_run_state::{
        CredentialGeneration, effect_writer_generation_role, effect_writer_scope_hash,
    };

    #[test]
    fn valid_project_ids_pass() {
        let max = "x".repeat(MAX_PROJECT_ID_LEN);
        for id in [
            "a",
            "acme",
            "acme-corp",
            "proj-1",
            "p9",
            "a-b-c",
            max.as_str(),
        ] {
            assert!(validate_project_id(id).is_ok(), "{id:?} should be valid");
        }
    }

    #[test]
    fn consecutive_hyphens_are_rejected() {
        // wamn-R27: `--` is the component separator in project_env_database_name
        // (and `__` in cdc_object_name), so an interior `--` run inside a slug is
        // an identity-collision vector and must be rejected. A SINGLE interior
        // hyphen stays valid.
        for bad in ["a--b", "x---y", "a--b--c", "ab--", "--ab"] {
            let err = validate_project_id(bad).unwrap_err();
            // "ab--"/"--ab" trip the start/end-alnum boundary first; the interior
            // cases trip the consecutive-hyphen rule — either way, rejected.
            assert!(
                matches!(err, ProvisionError::InvalidProjectId { .. }),
                "{bad:?} should be rejected"
            );
        }
        // The interior-run rule reports its own reason.
        assert!(matches!(
            validate_project_id("a--b"),
            Err(ProvisionError::InvalidProjectId {
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
        assert!(validate_project_id("a-b").is_ok());
    }

    #[test]
    fn invalid_project_ids_are_rejected_with_a_reason() {
        // empty / too long / charset / boundary
        assert!(matches!(
            validate_project_id(""),
            Err(ProvisionError::InvalidProjectId {
                reason: "empty",
                ..
            })
        ));
        assert!(matches!(
            validate_project_id(&"x".repeat(41)),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        for bad in [
            "Acme",
            "under_score",
            "has.dot",
            "space bar",
            "sql;--",
            "tab\tx",
        ] {
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::InvalidProjectId {
                        reason: "only lowercase letters, digits, and hyphens are allowed",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        for bad in ["-lead", "trail-", "-", "9-"] {
            // "9-" ends on a hyphen; "-lead"/"-" start on one.
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::InvalidProjectId {
                        reason: "must start and end with a lowercase letter or digit",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn reserved_wamn_prefix_is_rejected() {
        for bad in ["wamn", "wamn-db", "wamn-proj", "wamn-anything"] {
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::ReservedProjectId { .. })
                ),
                "{bad:?} should be reserved"
            );
        }
        // The boundary is a hyphen: `wamn` + non-hyphen is a normal project.
        assert!(validate_project_id("wamning").is_ok());
        assert!(validate_project_id("wamnable").is_ok());
    }

    #[test]
    fn names_derive_from_the_project_id() {
        assert_eq!(database_name("acme-corp"), "wamn-db-acme-corp");
        assert_eq!(secret_name("acme-corp"), "wamn-db-acme-corp");
        // Database and Secret names are identical (one lookup key for 5x0.1).
        assert_eq!(database_name("p1"), secret_name("p1"));
    }

    #[test]
    fn project_env_names_encode_the_full_triple() {
        // The org is encoded (unlike the 2.3 project-only name) so identically
        // named projects across orgs never collide on the shared T3 pool.
        assert_eq!(
            project_env_database_name("acme", "billing", "dev"),
            "wamn-db-acme--billing--dev"
        );
        assert_eq!(
            project_env_database_name("acme", "billing", "prod"),
            "wamn-db-acme--billing--prod"
        );
        // canary and prod are distinct names (they co-reside on <org>-prod, so the
        // env MUST be in the name to keep them apart).
        assert_ne!(
            project_env_database_name("acme", "billing", "canary"),
            project_env_database_name("acme", "billing", "prod")
        );
        // Two orgs, same project+env → different db names (the pool collision fix).
        assert_ne!(
            project_env_database_name("org-a", "demo", "dev"),
            project_env_database_name("org-b", "demo", "dev")
        );
        // Secret name == database name (one lookup key).
        assert_eq!(
            project_env_secret_name("acme", "billing", "dev"),
            project_env_database_name("acme", "billing", "dev")
        );
    }

    #[test]
    fn validate_project_env_slug_checks_and_bounds_the_name() {
        assert!(validate_project_env("acme", "billing", "prod").is_ok());
        // A bad project id is rejected (the reason flows from validate_project_id).
        assert!(matches!(
            validate_project_env("acme", "Bad", "dev"),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        assert!(matches!(
            validate_project_env("acme", "wamn-x", "dev"),
            Err(ProvisionError::ReservedProjectId { .. })
        ));
        // wamn-R27: the org and env are slug-checked too (not only the project).
        assert!(matches!(
            validate_project_env("Bad_Org", "billing", "dev"),
            Err(ProvisionError::InvalidComponent {
                component: "org",
                ..
            })
        ));
        assert!(matches!(
            validate_project_env("acme", "billing", "Bad_Env"),
            Err(ProvisionError::InvalidComponent {
                component: "env",
                ..
            })
        ));
        // The assembled name must fit the Postgres / DNS-1123 63-byte limit.
        let long_org = "o".repeat(40);
        let long_proj = "p".repeat(40);
        let err = validate_project_env(&long_org, &long_proj, "canary").unwrap_err();
        assert!(matches!(err, ProvisionError::NameTooLong { max: 63, .. }));
        // 44 combined bytes — the longest triple the namespace stem admits once
        // the instance suffix has taken its 10 bytes of the DNS-1123 label.
        assert!(validate_project_env(&"o".repeat(20), &"p".repeat(20), "prod").is_ok());
    }

    #[test]
    fn the_environment_namespace_carries_the_minted_instance_suffix() {
        // One environment = one namespace (wamn-0h0g.15.3): the same `--`
        // separated triple the database name carries, under the `wamn-` prefix,
        // plus the provision-minted instance suffix (wamn-0h0g.13.57).
        assert_eq!(
            project_env_namespace("acme", "billing", "dev", "k3m9x2p7"),
            "wamn-acme--billing--dev--k3m9x2p7"
        );
        // canary and prod are distinct namespaces, as they are distinct databases.
        assert_ne!(
            project_env_namespace("acme", "billing", "canary", "k3m9x2p7"),
            project_env_namespace("acme", "billing", "prod", "k3m9x2p7")
        );
        // THE POINT OF THE SUFFIX: the SAME triple provisioned a second time
        // lands in a DIFFERENT namespace, so a recreated environment cannot
        // inherit whatever survived the deleted one. Drop the suffix from the
        // derivation and these two collide.
        assert_ne!(
            project_env_namespace("acme", "billing", "prod", "k3m9x2p7"),
            project_env_namespace("acme", "billing", "prod", "q80zdw41")
        );
    }

    #[test]
    fn the_environment_namespace_is_a_valid_dns_1123_label() {
        // A Kubernetes namespace name is a DNS-1123 LABEL: at most 63 bytes,
        // `[a-z0-9-]`, starting and ending alphanumeric. Take the LONGEST triple
        // the validator admits (44 combined bytes) so the assembled label sits
        // exactly on the cap.
        let org = "o".repeat(40);
        assert!(validate_project_env(&org, "pp", "de").is_ok());
        let namespace = project_env_namespace(&org, "pp", "de", "0z9a8b7c");
        assert_eq!(namespace.len(), MAX_NAMESPACE_LEN);
        assert!(
            namespace
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "a DNS-1123 label admits only [a-z0-9-]: {namespace:?}"
        );
        assert!(
            !namespace.starts_with('-') && !namespace.ends_with('-'),
            "a DNS-1123 label starts and ends alphanumeric: {namespace:?}"
        );
        // The suffix decides the label's LAST byte, which is why a hyphen in it
        // is refused before it can name anything.
        assert!(validate_instance_suffix("k3m9x2p-").is_err());
    }

    #[test]
    fn instance_suffix_discipline_is_the_dns_label_tail() {
        assert!(validate_instance_suffix("k3m9x2p7").is_ok());
        assert!(validate_instance_suffix("00000000").is_ok());
        for bad in [
            "",
            "k3m9x2p",
            "k3m9x2p78",
            "K3M9X2P7",
            "k3m9x2p-",
            "k3m9x2p_",
        ] {
            assert!(
                matches!(
                    validate_instance_suffix(bad),
                    Err(ProvisionError::InvalidComponent {
                        component: "instance",
                        ..
                    })
                ),
                "{bad:?} should be rejected"
            );
        }
        // The two rejection reasons are distinct and stable.
        assert!(matches!(
            validate_instance_suffix("short"),
            Err(ProvisionError::InvalidComponent {
                reason: "must be exactly 8 bytes",
                ..
            })
        ));
        assert!(matches!(
            validate_instance_suffix("k3m9x2p-"),
            Err(ProvisionError::InvalidComponent {
                reason: "only lowercase letters and digits are allowed",
                ..
            })
        ));
    }

    #[test]
    fn the_namespace_bound_accounts_for_the_instance_suffix() {
        // The refusal formula must budget for the suffix, not just the triple.
        // org(40) + project(2) + env(3) = 45 combined bytes: the stem is 54 bytes
        // and would fit the 63-byte label on its own, but the namespace it
        // actually names is 64. Drop INSTANCE_SUFFIX_LEN from
        // MAX_NAMESPACE_STEM_LEN and this triple is wrongly admitted.
        let org = "o".repeat(40);
        let stem = project_env_namespace_stem(&org, "pp", "dev");
        assert_eq!(stem.len(), 54);
        assert!(stem.len() <= MAX_NAMESPACE_LEN, "fits WITHOUT the suffix");
        assert_eq!(
            project_env_namespace(&org, "pp", "dev", "k3m9x2p7").len(),
            MAX_NAMESPACE_LEN + 1,
            "and does NOT fit WITH it"
        );
        match validate_project_env(&org, "pp", "dev") {
            Err(ProvisionError::NameTooLong { name, max }) => {
                assert_eq!(name, stem);
                assert_eq!(max, MAX_NAMESPACE_STEM_LEN);
            }
            other => panic!("the suffixed namespace bound must refuse this triple, got {other:?}"),
        }
        // One byte shorter and the assembled namespace sits exactly on the cap.
        assert!(validate_project_env(&org, "pp", "de").is_ok());
        assert_eq!(
            project_env_namespace(&org, "pp", "de", "k3m9x2p7").len(),
            MAX_NAMESPACE_LEN
        );
    }

    #[test]
    fn both_assembled_name_bounds_stay_reachable() {
        // wamn-0h0g.15.72's property, preserved through the suffix: the LOOSER
        // bound is checked FIRST, so neither branch is dead code. The suffix
        // inverted which is looser — the namespace stem leaves the triple 53
        // bytes, the database name 51 — so the database check now runs first.
        let org = "o".repeat(40);

        // 52 combined bytes: the database name is 64 and refuses. Under a
        // namespace-first order this would report the stem instead.
        let over_db = "p".repeat(9);
        match validate_project_env(&org, &over_db, "dev") {
            Err(ProvisionError::NameTooLong { name, max }) => {
                assert_eq!(name, project_env_database_name(&org, &over_db, "dev"));
                assert_eq!(max, MAX_DB_NAME_LEN);
            }
            other => panic!("the database bound must refuse a 52-byte triple, got {other:?}"),
        }

        // 51 combined bytes: the database name is exactly 63 and PASSES, so only
        // the namespace can refuse — the branch that goes dead if the order is
        // chosen wrongly.
        let over_namespace = "p".repeat(8);
        assert_eq!(
            project_env_database_name(&org, &over_namespace, "dev").len(),
            MAX_DB_NAME_LEN
        );
        match validate_project_env(&org, &over_namespace, "dev") {
            Err(ProvisionError::NameTooLong { name, max }) => {
                assert_eq!(
                    name,
                    project_env_namespace_stem(&org, &over_namespace, "dev")
                );
                assert_eq!(max, MAX_NAMESPACE_STEM_LEN);
            }
            other => panic!("the namespace bound must refuse a 51-byte triple, got {other:?}"),
        }
    }

    #[test]
    fn double_hyphen_collision_pair_is_rejected_in_every_component() {
        // The wamn-R27 collision: `(a, x--p, dev)` and `(a--x, p, dev)` both
        // derive `wamn-db-a--x--p--dev` (and `wamn_cdc_a__x__p__dev`). The
        // derivations still alias — that is why the VALIDATOR is the fix: both
        // triples are now rejected before they can be provisioned.
        assert_eq!(
            project_env_database_name("a", "x--p", "dev"),
            project_env_database_name("a--x", "p", "dev"),
            "the derivations still alias — rejection is what closes the collision"
        );
        assert_eq!(
            cdc_object_name("a", "x--p", "dev"),
            cdc_object_name("a--x", "p", "dev")
        );
        // Project carries the `--`: rejected via validate_project_id.
        assert!(matches!(
            validate_project_env("a", "x--p", "dev"),
            Err(ProvisionError::InvalidProjectId {
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
        // Org carries the `--`: rejected as the org component.
        assert!(matches!(
            validate_project_env("a--x", "p", "dev"),
            Err(ProvisionError::InvalidComponent {
                component: "org",
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
        // Env carries the `--`: rejected as the env component.
        assert!(matches!(
            validate_project_env("a", "p", "d--v"),
            Err(ProvisionError::InvalidComponent {
                component: "env",
                reason: "must not contain consecutive hyphens",
                ..
            })
        ));
    }

    #[test]
    fn derived_names_are_injective_over_valid_triples() {
        // Enumerate short slugs across the three components; every VALID triple
        // must map to a UNIQUE database name AND a unique CDC object name. The
        // alphabet deliberately includes `--` slugs — `("a","b--c")` and
        // `("a--b","c")` both derive `wamn-db-a--b--c--<env>` (and
        // `wamn_cdc_a__b__c__<env>`). With the `--` ban they are FILTERED out (so
        // injectivity holds over the survivors); remove the ban and both survive
        // and collide here — this test is also a mutation catcher for the ban.
        let slugs = ["a", "b", "a-b", "c", "a--b", "b--c"];
        let envs = ["dev", "prod", "c-d"];
        let mut by_db: std::collections::HashMap<String, (&str, &str, &str)> =
            std::collections::HashMap::new();
        let mut by_cdc: std::collections::HashMap<String, (&str, &str, &str)> =
            std::collections::HashMap::new();
        let mut valid = 0usize;
        for &org in &slugs {
            for &project in &slugs {
                for &env in &envs {
                    // Only VALID triples derive names; `--`/`_` slugs never reach here.
                    if validate_project_env_cdc(org, project, env).is_err() {
                        continue;
                    }
                    valid += 1;
                    let triple = (org, project, env);
                    let db = project_env_database_name(org, project, env);
                    if let Some(prev) = by_db.insert(db.clone(), triple) {
                        assert_eq!(prev, triple, "database name {db:?} collides two triples");
                    }
                    let cdc = cdc_object_name(org, project, env);
                    if let Some(prev) = by_cdc.insert(cdc.clone(), triple) {
                        assert_eq!(prev, triple, "cdc object name {cdc:?} collides two triples");
                    }
                }
            }
        }
        // The `--` slugs (2 of 6 org/project values) are filtered by the ban, so
        // exactly the 4 hyphen-run-free slugs survive per id position × 3 envs.
        // The count being BELOW the full product proves the filter actually ran.
        assert!(
            valid < slugs.len() * slugs.len() * envs.len(),
            "the `--` filter ran"
        );
        assert_eq!(
            valid,
            4 * 4 * 3,
            "4 valid org × 4 valid project × 3 valid env"
        );
        assert_eq!(by_db.len(), valid, "database names are injective");
        assert_eq!(by_cdc.len(), valid, "cdc object names are injective");
    }

    #[test]
    fn underscore_is_not_a_second_collision_vector() {
        // cdc_object_name maps `-`→`_`; a `_` INSIDE a slug would be a second way
        // to forge the `__` separator. The slug charset admits no underscore, so
        // the vector is closed at validation — confirm it here.
        assert!(matches!(
            validate_project_id("a_b"),
            Err(ProvisionError::InvalidProjectId {
                reason: "only lowercase letters, digits, and hyphens are allowed",
                ..
            })
        ));
        assert!(matches!(
            validate_project_env("a_b", "p", "dev"),
            Err(ProvisionError::InvalidComponent {
                component: "org",
                ..
            })
        ));
        assert!(matches!(
            validate_project_env("a", "p", "d_v"),
            Err(ProvisionError::InvalidComponent {
                component: "env",
                ..
            })
        ));
    }

    #[test]
    fn cdc_names_encode_the_triple_in_both_domains() {
        // The Secret keeps the hyphenated K8s convention…
        assert_eq!(
            project_env_cdc_secret_name("acme", "billing", "dev"),
            "wamn-cdc-acme--billing--dev"
        );
        // …while the Postgres objects (slot charset `[a-z0-9_]`) are underscored,
        // slug hyphens mapped to `_`, `__` as the separator.
        assert_eq!(
            cdc_object_name("acme", "billing", "dev"),
            "wamn_cdc_acme__billing__dev"
        );
        assert_eq!(
            cdc_object_name("org-a", "demo", "dev"),
            "wamn_cdc_org_a__demo__dev"
        );
        // A hyphen inside a slug maps to a SINGLE `_`, the separator is DOUBLE —
        // ("org-a","demo") and ("org","a-demo") stay distinct.
        assert_ne!(
            cdc_object_name("org-a", "demo", "dev"),
            cdc_object_name("org", "a-demo", "dev")
        );
        // The stream name follows the D19 v3 contract.
        assert_eq!(event_stream_name("acme", "prod"), "EVT_acme_prod");
        assert_eq!(event_stream_name("org-a", "dev"), "EVT_org-a_dev");
    }

    #[test]
    fn validate_project_env_cdc_bounds_the_object_name() {
        assert!(validate_project_env_cdc("acme", "billing", "prod").is_ok());
        // The base project-env validation still applies (slug + reserved rules).
        assert!(matches!(
            validate_project_env_cdc("acme", "Bad", "dev"),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        // The CDC object name is one byte per component longer than the db name
        // ("wamn_cdc_" = 9 vs "wamn-db-" = 8), so it bounds a 50-byte triple
        // where the db name bounds a 51-byte one. Since the namespace stem
        // refuses anything over 44 (wamn-0h0g.13.57), the base validation always
        // fires first and this extra branch is SUBSUMED — it binds again once the
        // CDC objects carry the instance suffix too. Pin what is true: a triple
        // whose db name sits exactly on the limit is still refused…
        let org = "o".repeat(25);
        let project = "p".repeat(22);
        assert_eq!(
            project_env_database_name(&org, &project, "prod").len(),
            MAX_DB_NAME_LEN
        );
        assert!(matches!(
            validate_project_env_cdc(&org, &project, "prod"),
            Err(ProvisionError::NameTooLong { .. })
        ));
        // …and every triple the base validation admits has a legal CDC name.
        let org_ok = "o".repeat(40);
        assert!(validate_project_env_cdc(&org_ok, "pp", "de").is_ok());
        assert!(cdc_object_name(&org_ok, "pp", "de").len() <= MAX_DB_NAME_LEN);
    }

    #[test]
    fn compose_url_percent_encodes_userinfo() {
        let u = compose_url("wamn_app", "wamn_app", "wamn-pg-rw", 5432, "wamn-db-acme");
        assert_eq!(
            u,
            "postgres://wamn_app:wamn_app@wamn-pg-rw:5432/wamn-db-acme"
        );
        // A password with URL-reserved characters is encoded, not injected.
        let u = compose_url("wamn_app", "p@ss:w/rd", "h", 5432, "wamn-db-x");
        assert_eq!(u, "postgres://wamn_app:p%40ss%3Aw%2Frd@h:5432/wamn-db-x");
        // The hyphenated database name is URL-unreserved (no encoding).
        assert!(u.ends_with("/wamn-db-x"));
    }

    #[test]
    fn effect_writer_scope_identity_and_names_are_frozen() {
        let database = project_env_database_name("acme", "billing", "dev");
        assert_eq!(
            effect_writer_scope_hash("acme", "billing", "dev", &database),
            "1bf626624be49afafc549ec9c5561e146d0f6c33"
        );
        let a = effect_writer_generation_role(
            "acme",
            "billing",
            "dev",
            &database,
            CredentialGeneration::A,
        );
        let b = effect_writer_generation_role(
            "acme",
            "billing",
            "dev",
            &database,
            CredentialGeneration::B,
        );
        assert_eq!(
            a,
            "wamn_effect_writer_1bf626624be49afafc549ec9c5561e146d0f6c33_a"
        );
        assert_eq!(
            b,
            "wamn_effect_writer_1bf626624be49afafc549ec9c5561e146d0f6c33_b"
        );
        assert_eq!(a.len(), 61);
        assert_eq!(b.len(), 61);
        assert_eq!(CredentialGeneration::A.other(), CredentialGeneration::B);
        assert_eq!(CredentialGeneration::B.other(), CredentialGeneration::A);
        assert_eq!(
            project_env_effect_writer_secret_name("acme", "billing", "dev"),
            "wamn-effect-writer-acme--billing--dev"
        );
    }

    #[test]
    fn effect_writer_scope_hash_frames_every_identity_component() {
        let base = effect_writer_scope_hash("acme", "billing", "dev", "db");
        for changed in [
            effect_writer_scope_hash("acme-x", "billing", "dev", "db"),
            effect_writer_scope_hash("acme", "billing-x", "dev", "db"),
            effect_writer_scope_hash("acme", "billing", "prod", "db"),
            effect_writer_scope_hash("acme", "billing", "dev", "db-x"),
        ] {
            assert_ne!(base, changed);
            assert_eq!(changed.len(), 40);
            assert!(
                changed
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            );
        }
    }
}
