//! Per-project credential emission — the artifact 2.2b (`K8sSecretProvider`,
//! wamn-5x0.1) will consume.
//!
//! 2.3 **emits** the credential; the live in-cluster read stays 5x0.1. Two
//! shapes, both pure JSON:
//!
//! * [`projects_file`] / [`projects_file_entry`] — the `WAMN_PG_PROJECTS_FILE`
//!   format the plugin's `StaticCredentialProvider` already parses (`{ project:
//!   { "url": … } }`), so a provisioned project resolves through the exact code
//!   path production uses.
//! * [`render_secret_manifest`] — a Kubernetes `Secret` manifest (rendered as
//!   JSON, which `kubectl apply -f` accepts), named `wamn-db-<project>` — the
//!   lookup key 5x0.1 reads. `stringData.url` is the app-role connection URL.

use serde_json::{Value, json};
use wamn_control_registry::Triple;
use wamn_run_state::{EFFECT_WRITER_CREDENTIAL_KEY, EffectWriterCredential};

use crate::name::{
    APP_ROLE, cdc_object_name, project_env_cdc_secret_name, project_env_secret_name, secret_name,
    workload_secret_name,
};
use crate::workload_role::{WorkloadRoleFamily, WorkloadSecretBodyKind};

/// The `WAMN_PG_PROJECTS_FILE` entry for one project: `{ "url": <url> }`.
/// Policy knobs (`row_limit`, timeouts) are optional and default from the
/// plugin's base config, so the MVP entry carries only the URL.
pub fn projects_file_entry(url: &str) -> Value {
    json!({ "url": url })
}

/// A complete single-project `WAMN_PG_PROJECTS_FILE` object: `{ <project>: {
/// "url": <url> } }`.
pub fn projects_file(project: &str, url: &str) -> Value {
    json!({ project: projects_file_entry(url) })
}

/// Render the per-project credential `Secret` as a JSON manifest. Name
/// `wamn-db-<project>`; `stringData` carries the app-role URL (and the project
/// id + role for readability). `kubectl apply -f` accepts JSON, so the
/// provisioning Job can pipe this straight to the API server without a Rust K8s
/// client (that write path is deliberately kept out of 2.3 — see the crate docs).
pub fn render_secret_manifest(project: &str, namespace: &str, url: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name(project),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-db-credentials",
                "wamn.project": project,
            },
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "project": project,
            "role": APP_ROLE,
        },
    })
}

/// Render the per-project-env credential `Secret` (wamn-q3n.7). Name
/// `wamn-db-<org>--<project>--<env>` — the 5x0.1 lookup key recorded as the
/// project-env's `SecretRef` in the registry. `stringData.url` is the app-role
/// connection URL to the project-env database; the labels carry the full identity
/// triple so tooling never parses the name.
pub fn render_project_env_secret_manifest(triple: &Triple, namespace: &str, url: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": project_env_secret_name(&triple.org, &triple.project, triple.env.as_str()),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-db-credentials",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "org": triple.org,
            "project": triple.project,
            "env": triple.env.as_str(),
            "role": APP_ROLE,
        },
    })
}

/// The body one workload credential `Secret` carries.
///
/// Three SHAPES, closed — not one variant per family. Which shape a family
/// takes is [`WorkloadRoleFamily::secret_body_kind`], so an admitted family
/// gets the plain single-`url` Secret with no edit here.
#[derive(Debug, Clone, Copy)]
pub enum WorkloadSecretBody<'a> {
    /// The single `url` key every consumer mounts through
    /// `secretKeyRef … key: url`.
    Url(&'a str),
    /// The same single `url`, plus the tenant key as a label and the tenant id
    /// as an annotation (`wamn-0h0g.22.6.4`).
    ///
    /// The TENANT KEY is a label and the tenant id an ANNOTATION deliberately:
    /// a label value is capped at 63 characters and restricted to alphanumerics
    /// plus `-_.`, while `valid_tenant` admits 64 bytes — so a label carrying
    /// the tenant verbatim would be rejected by the API server for exactly the
    /// tenants the digest exists to handle.
    TenantUrl {
        tenant: &'a str,
        tenant_key: &'a str,
        url: &'a str,
    },
    /// The frozen effect-writer `credential.json` document.
    ///
    /// Kubernetes mounts the whole Secret directory read-only, without
    /// `subPath`, so an atomic Secret projection update can be observed after
    /// the wrapper drains/reloads pools.
    EffectWriterCredential(&'a EffectWriterCredential),
}

impl WorkloadSecretBody<'_> {
    fn kind(self) -> WorkloadSecretBodyKind {
        match self {
            Self::Url(_) => WorkloadSecretBodyKind::Url,
            Self::TenantUrl { .. } => WorkloadSecretBodyKind::TenantUrl,
            Self::EffectWriterCredential(_) => WorkloadSecretBodyKind::EffectWriterCredential,
        }
    }
}

/// ONE workload credential `Secret` renderer, for any family
/// (`wamn-0h0g.22.16`).
///
/// Replaces the four copy-pasted per-family renderers. Name, component label
/// and body shape are all DERIVED from the family, so admitting a family
/// publishes its Secret without a renderer being written for it. The body is
/// checked against the family's declared shape rather than trusted, so a caller
/// cannot hand the guest family a plain url and lose the tenant key.
pub fn render_workload_secret_manifest(
    family: WorkloadRoleFamily,
    triple: &Triple,
    namespace: &str,
    body: WorkloadSecretBody<'_>,
) -> Value {
    assert_eq!(
        body.kind(),
        family.secret_body_kind(),
        "{family:?} publishes a {:?} Secret body, not a {:?} one",
        family.secret_body_kind(),
        body.kind(),
    );
    let mut metadata = json!({
        "name": workload_secret_name(family, &triple.org, &triple.project, triple.env.as_str()),
        "namespace": namespace,
        "labels": {
            "app.kubernetes.io/managed-by": "wamn",
            "app.kubernetes.io/component": format!("{}-credentials", family.component_stem()),
            "wamn.org": triple.org,
            "wamn.project": triple.project,
            "wamn.env": triple.env.as_str(),
        },
    });
    let string_data = match body {
        WorkloadSecretBody::Url(url) => json!({ "url": url }),
        WorkloadSecretBody::TenantUrl {
            tenant,
            tenant_key,
            url,
        } => {
            metadata["labels"]["wamn.tenant-key"] = json!(tenant_key);
            metadata["annotations"] = json!({ "wamn.io/tenant": tenant });
            json!({ "url": url })
        }
        WorkloadSecretBody::EffectWriterCredential(credential) => {
            metadata["annotations"] = Value::Object(effect_writer_annotations(credential));
            json!({
                (EFFECT_WRITER_CREDENTIAL_KEY): serde_json::to_string(credential)
                    .expect("effect-writer credential serializes"),
            })
        }
    };
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": metadata,
        "type": "Opaque",
        "stringData": string_data,
    })
}

/// The effect-writer credential's own annotation block — the one family whose
/// Secret carries a frozen document rather than a url.
fn effect_writer_annotations(
    credential: &EffectWriterCredential,
) -> serde_json::Map<String, Value> {
    let document = serde_json::to_value(credential).expect("effect-writer credential serializes");
    let field = |name: &str| {
        document[name]
            .as_str()
            .unwrap_or_else(|| panic!("effect-writer credential {name} is a string"))
    };
    let mut annotations = serde_json::Map::from_iter([
        (
            "wamn.io/credential-id".to_string(),
            Value::String(credential.credential_id().to_string()),
        ),
        (
            "wamn.io/credential-generation".to_string(),
            Value::String(credential.generation().as_str().to_string()),
        ),
        (
            "wamn.io/database-role".to_string(),
            Value::String(credential.role().to_string()),
        ),
        (
            "wamn.io/tenant".to_string(),
            Value::String(field("tenant").to_string()),
        ),
        (
            "wamn.io/issued-at".to_string(),
            Value::String(field("issued-at").to_string()),
        ),
        (
            "wamn.io/not-before".to_string(),
            Value::String(field("not-before").to_string()),
        ),
        (
            "wamn.io/expires-at".to_string(),
            Value::String(field("expires-at").to_string()),
        ),
    ]);
    if let Some(revoked_at) = document["revoked-at"].as_str() {
        annotations.insert(
            "wamn.io/revoked-at".to_string(),
            Value::String(revoked_at.to_string()),
        );
    }
    annotations
}

/// Render the fixed-mount effect-writer Secret for one credential generation.
pub fn render_effect_writer_secret_manifest(
    triple: &Triple,
    namespace: &str,
    credential: &EffectWriterCredential,
) -> Value {
    render_workload_secret_manifest(
        WorkloadRoleFamily::EffectWriter,
        triple,
        namespace,
        WorkloadSecretBody::EffectWriterCredential(credential),
    )
}

/// Render the scoped control-author URL Secret consumed by scenario-worker.
pub fn render_control_author_secret_manifest(triple: &Triple, namespace: &str, url: &str) -> Value {
    render_workload_secret_manifest(
        WorkloadRoleFamily::ControlAuthor,
        triple,
        namespace,
        WorkloadSecretBody::Url(url),
    )
}

/// Render the scoped management-admitter URL Secret consumed by scenario-worker.
///
/// The sibling of [`render_control_author_secret_manifest`] on the other plane:
/// control-author's URL names the **control** database, this one names the
/// **project environment's own** database. Both carry the single `url` key
/// because scenario-worker reads both through the same `secretKeyRef … key: url`
/// shape (`wamn-0h0g.8.5.3`).
pub fn render_management_admitter_secret_manifest(
    triple: &Triple,
    namespace: &str,
    url: &str,
) -> Value {
    render_workload_secret_manifest(
        WorkloadRoleFamily::ManagementAdmitter,
        triple,
        namespace,
        WorkloadSecretBody::Url(url),
    )
}

/// Render the scoped per-tenant guest-SQL credential `Secret`
/// (`wamn-0h0g.22.6.4`).
///
/// `stringData.url` names the tenant's own LOGIN generation, which is the whole
/// point: after the `wamn-0h0g.22.6` sweep the guest's tenant comes from
/// `current_user`, so the credential IS the tenant authority and no claim
/// accompanies it.
pub fn render_guest_secret_manifest(
    triple: &Triple,
    namespace: &str,
    tenant: &str,
    tenant_key: &str,
    url: &str,
) -> Value {
    render_workload_secret_manifest(
        WorkloadRoleFamily::App,
        triple,
        namespace,
        WorkloadSecretBody::TenantUrl {
            tenant,
            tenant_key,
            url,
        },
    )
}

/// Render the per-project-env **CDC** credential `Secret` (wamn-l5i9.9). Name
/// `wamn-cdc-<org>--<project>--<env>` — the reference the reader registration
/// records as `replication_secret_name`, DISTINCT from the `wamn-db-…` query
/// Secret (the replication credential is its own R8b tier). `stringData.url` is
/// the replication-role connection URL to the project-env database (a plain
/// libpq URL; the reader appends its own connection parameters, e.g.
/// `replication=database`, when it opens the walsender session — l5i9.10).
pub fn render_project_env_cdc_secret_manifest(
    triple: &Triple,
    instance: &str,
    namespace: &str,
    url: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": project_env_cdc_secret_name(&triple.org, &triple.project, triple.env.as_str()),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-cdc-credentials",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "org": triple.org,
            "project": triple.project,
            "env": triple.env.as_str(),
            "role": cdc_object_name(
                &triple.org,
                &triple.project,
                triple.env.as_str(),
                instance,
            ),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    use chrono::{DateTime, Utc};
    use wamn_run_state::{
        CredentialGeneration, EFFECT_WRITER_CREDENTIAL_PATH, EffectWriterCredentialScope,
        EffectWriterCredentialValidity, effect_writer_credential, effect_writer_generation_role,
        parse_effect_writer_credential, validate_effect_writer_credential,
    };

    const URL: &str = "postgres://wamn_app:wamn_app@wamn-pg-rw:5432/wamn-db-acme";

    fn writer_fixture() -> (Triple, EffectWriterCredentialScope, EffectWriterCredential) {
        let triple = Triple::new("acme", "billing", "dev");
        let scope = EffectWriterCredentialScope {
            tenant: "tenant".to_string(),
            org: "acme".to_string(),
            project: "billing".to_string(),
            environment: "dev".to_string(),
            database: "wamn-db-acme--billing--dev".to_string(),
        };
        let role =
            effect_writer_generation_role(&scope.tenant, &scope.database, CredentialGeneration::A);
        let credential = effect_writer_credential(
            &scope,
            "0123456789abcdef0123456789abcdef",
            CredentialGeneration::A,
            &EffectWriterCredentialValidity {
                issued_at: "2026-01-01T00:00:00Z".to_string(),
                not_before: "2026-01-01T00:00:00Z".to_string(),
                expires_at: "2026-02-01T00:00:00Z".to_string(),
                revoked_at: None,
            },
            &format!(
                "postgres://{role}:{}@wamn-pg-rw:5432/{}",
                "a".repeat(64),
                scope.database
            ),
        );
        (triple, scope, credential)
    }

    fn instant(value: &str) -> SystemTime {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
            .into()
    }

    #[test]
    fn projects_file_matches_the_plugin_parse_shape() {
        // `StaticCredentialProvider::projects_from_json` expects
        // `{ "<project>": { "url": "…" } }`.
        let pf = projects_file("acme", URL);
        assert_eq!(pf["acme"]["url"], URL);
        assert_eq!(projects_file_entry(URL)["url"], URL);
    }

    #[test]
    fn secret_manifest_has_the_layout_5x0_1_reads() {
        let s = render_secret_manifest("acme", "wamn-system", URL);
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "wamn-db-acme");
        assert_eq!(s["metadata"]["namespace"], "wamn-system");
        assert_eq!(s["metadata"]["labels"]["wamn.project"], "acme");
        assert_eq!(s["type"], "Opaque");
        assert_eq!(s["stringData"]["url"], URL);
        assert_eq!(s["stringData"]["project"], "acme");
        assert_eq!(s["stringData"]["role"], "wamn_app");
    }

    #[test]
    fn project_env_secret_names_and_labels_carry_the_triple() {
        let t = Triple::new("acme", "billing", "dev");
        let url = "postgres://wamn_app:wamn_app@acme-dev-rw:5432/wamn-db-acme--billing--dev";
        let s = render_project_env_secret_manifest(&t, "wamn-system", url);
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "wamn-db-acme--billing--dev");
        assert_eq!(s["metadata"]["namespace"], "wamn-system");
        assert_eq!(s["metadata"]["labels"]["wamn.org"], "acme");
        assert_eq!(s["metadata"]["labels"]["wamn.project"], "billing");
        assert_eq!(s["metadata"]["labels"]["wamn.env"], "dev");
        assert_eq!(s["stringData"]["url"], url);
        assert_eq!(s["stringData"]["org"], "acme");
        assert_eq!(s["stringData"]["project"], "billing");
        assert_eq!(s["stringData"]["env"], "dev");
        assert_eq!(s["stringData"]["role"], "wamn_app");
    }

    #[test]
    fn control_author_secret_matches_the_scenario_worker_mount() {
        let triple = Triple::new("acme", "receiving", "dev");
        let url = "postgres://wamn_control_author_scope_a:secret@control-rw/wamn-system";
        let secret = render_control_author_secret_manifest(&triple, "wamn-system", url);
        assert_eq!(
            secret["metadata"]["name"],
            "wamn-authoring-acme--receiving--dev"
        );
        assert_eq!(secret["stringData"]["url"], url);
    }

    /// `wamn-0h0g.8.5.3` landed the consuming half — a `secretKeyRef` in
    /// `deploy/platform/scenario-worker.yaml` — before anything minted the
    /// Secret, and measured that pointing that reference at a wrong name killed
    /// no test. `tests/conformance`'s workload scanner filters to kind
    /// `WorkloadDeployment` and scenario-worker is a plain `Deployment`, so it is
    /// invisible there.
    ///
    /// This is the equality that closes it, and it is not a source scan: the
    /// Deployment's `secretKeyRef` name and key are compared against the
    /// **renderer's own output** for the same scope, so drifting either half
    /// fails here.
    #[test]
    fn management_admitter_secret_is_mount_exact_with_the_scenario_worker_deployment() {
        const SCENARIO_WORKER: &str =
            include_str!("../../../../deploy/platform/scenario-worker.yaml");

        let triple = Triple::new("acme", "receiving", "dev");
        let url = "postgres://wamn_mgmt_admitter_scope_a:pw@acme-dev-rw:5432/\
                   wamn-db-acme--receiving--dev--k3m9x2p7";
        let secret = render_management_admitter_secret_manifest(&triple, "wamn-system", url);
        assert_eq!(secret["kind"], "Secret");
        assert_eq!(secret["type"], "Opaque");
        assert_eq!(
            secret["metadata"]["labels"]["app.kubernetes.io/component"],
            "management-admitter-credentials"
        );
        assert_eq!(secret["metadata"]["namespace"], "wamn-system");
        // R8b: its own credential tier is its own Secret — never the control
        // database's authoring Secret, which the same pod also mounts.
        assert_ne!(
            secret["metadata"]["name"],
            render_control_author_secret_manifest(&triple, "wamn-system", url)["metadata"]["name"]
        );

        // Exactly one key, whatever it is named, and it carries the URL.
        let data = secret["stringData"].as_object().unwrap();
        assert_eq!(data.len(), 1);
        let key = data.keys().next().expect("the Secret carries one key");
        assert_eq!(data[key], url);
        let name = secret["metadata"]["name"]
            .as_str()
            .expect("the Secret is named");

        // The consuming env entry, read out of the Deployment rather than
        // restated: `valueFrom.secretKeyRef.{name,key}` must be exactly what the
        // renderer produced for this triple.
        let reference: Vec<String> = SCENARIO_WORKER
            .split("- name: WAMN_MANAGEMENT_ADMISSION_PG_URL")
            .nth(1)
            .expect("scenario-worker consumes WAMN_MANAGEMENT_ADMISSION_PG_URL")
            .lines()
            .skip(1)
            .take(4)
            .map(|line| line.trim().to_string())
            .collect();
        assert_eq!(
            reference,
            [
                "valueFrom:".to_string(),
                "secretKeyRef:".to_string(),
                format!("name: {name}"),
                format!("key: {key}"),
            ]
        );
    }

    /// THE CONSUMER-WIRING GUARD for the identity read (`wamn-0h0g.12.67`).
    ///
    /// The mount-exact shape above, applied to the credential that mattered
    /// most: `WAMN_SYSTEM_URL` named `wamn-system-db`, which authenticates as
    /// `wamn_system` — the owner of `identity.pats` and `identity.project_roles`
    /// under no row-level security. Re-pointing the reference back at that
    /// Secret, or at either of the other two this pod already mounts, fails
    /// here, because the name is compared against the RENDERER'S OWN output for
    /// this scope rather than restated.
    #[test]
    fn identity_reader_secret_is_mount_exact_with_the_scenario_worker_deployment() {
        const SCENARIO_WORKER: &str =
            include_str!("../../../../deploy/platform/scenario-worker.yaml");

        let triple = Triple::new("acme", "receiving", "dev");
        let url = "postgres://wamn_identity_reader_scope_a:pw@wamn-sysdb-rw:5432/wamn_system";
        let secret = render_workload_secret_manifest(
            WorkloadRoleFamily::IdentityReader,
            &triple,
            "wamn-system",
            WorkloadSecretBody::Url(url),
        );
        assert_eq!(secret["kind"], "Secret");
        assert_eq!(
            secret["metadata"]["labels"]["app.kubernetes.io/component"],
            "identity-reader-credentials"
        );
        let data = secret["stringData"].as_object().unwrap();
        assert_eq!(data.len(), 1);
        let key = data.keys().next().expect("the Secret carries one key");
        assert_eq!(data[key], url);
        let name = secret["metadata"]["name"]
            .as_str()
            .expect("the Secret is named");

        // R8b: three credential tiers, three Secrets. This one is never either
        // of the two the same pod already mounts.
        for other in [
            render_control_author_secret_manifest(&triple, "wamn-system", url),
            render_management_admitter_secret_manifest(&triple, "wamn-system", url),
        ] {
            assert_ne!(secret["metadata"]["name"], other["metadata"]["name"]);
        }
        // …and it is never the wide owner credential it replaced.
        assert_ne!(name, "wamn-system-db");

        let reference: Vec<String> = SCENARIO_WORKER
            .split("- name: WAMN_SYSTEM_URL")
            .nth(1)
            .expect("scenario-worker consumes WAMN_SYSTEM_URL")
            .lines()
            .skip(1)
            .take(4)
            .map(|line| line.trim().to_string())
            .collect();
        assert_eq!(
            reference,
            [
                "valueFrom:".to_string(),
                "secretKeyRef:".to_string(),
                format!("name: {name}"),
                format!("key: {key}"),
            ]
        );
        // The wide owner credential must be gone from the MOUNTS, not merely
        // unmentioned: the header still names it to say what was replaced.
        assert!(
            !SCENARIO_WORKER.contains("name: wamn-system-db"),
            "the deployment still mounts the unconfined wamn_system owner credential"
        );
    }

    #[test]
    fn cdc_secret_is_a_distinct_replication_tier_reference() {
        let t = Triple::new("acme", "billing", "dev");
        let url =
            "postgres://wamn_cdc_acme__billing__dev:pw@acme-dev-rw:5432/wamn-db-acme--billing--dev";
        let s = render_project_env_cdc_secret_manifest(&t, "k3m9x2p7", "wamn-system", url);
        assert_eq!(s["kind"], "Secret");
        // The CDC Secret name is the wamn-cdc-… sibling — NEVER the wamn-db-…
        // query Secret (a distinct R8b credential tier, one lookup key each).
        assert_eq!(s["metadata"]["name"], "wamn-cdc-acme--billing--dev");
        assert_ne!(
            s["metadata"]["name"],
            render_project_env_secret_manifest(&t, "wamn-system", url)["metadata"]["name"]
        );
        assert_eq!(
            s["metadata"]["labels"]["app.kubernetes.io/component"],
            "project-env-cdc-credentials"
        );
        assert_eq!(s["metadata"]["labels"]["wamn.org"], "acme");
        assert_eq!(s["metadata"]["labels"]["wamn.env"], "dev");
        assert_eq!(s["stringData"]["url"], url);
        // The role recorded is the underscored replication role, not wamn_app.
        assert_eq!(
            s["stringData"]["role"],
            "wamn_cdc_acme__billing__dev__k3m9x2p7"
        );
    }

    #[test]
    fn effect_writer_secret_and_document_are_fixed_mount_exact() {
        let (triple, scope, credential) = writer_fixture();
        validate_effect_writer_credential(&credential, &scope, instant("2026-01-15T00:00:00Z"))
            .unwrap();
        let secret = render_effect_writer_secret_manifest(&triple, "wamn-system", &credential);
        assert_eq!(
            secret["metadata"]["name"],
            "wamn-effect-writer-acme--billing--dev"
        );
        assert_eq!(
            secret["metadata"]["labels"]["app.kubernetes.io/component"],
            "effect-writer-credentials"
        );
        assert_eq!(
            secret["metadata"]["annotations"].as_object().unwrap().len(),
            7
        );
        assert_eq!(
            secret["metadata"]["annotations"]["wamn.io/credential-id"],
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            secret["metadata"]["annotations"]["wamn.io/issued-at"],
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            secret["metadata"]["annotations"]["wamn.io/tenant"],
            "tenant"
        );
        assert_eq!(
            secret["metadata"]["annotations"]["wamn.io/not-before"],
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            secret["metadata"]["annotations"]["wamn.io/expires-at"],
            "2026-02-01T00:00:00Z"
        );
        assert!(
            secret["metadata"]["annotations"]
                .get("wamn.io/revoked-at")
                .is_none()
        );
        let data = secret["stringData"].as_object().unwrap();
        assert_eq!(data.len(), 1);
        let parsed = parse_effect_writer_credential(
            data[EFFECT_WRITER_CREDENTIAL_KEY]
                .as_str()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(parsed, credential);
        let document: Value = serde_json::from_str(
            data[EFFECT_WRITER_CREDENTIAL_KEY]
                .as_str()
                .expect("credential.json stringData"),
        )
        .unwrap();
        assert!(document.get("schema").is_none());
        assert_eq!(
            EFFECT_WRITER_CREDENTIAL_PATH,
            "/etc/wamn/effect-writer/credential.json"
        );
        assert!(!format!("{credential:?}").contains(&"a".repeat(64)));
    }
}
