//! Per-project-env credential emission — the artifact 2.2b (`K8sSecretProvider`,
//! wamn-5x0.1) will consume.
//!
//! Provisioning **emits** the credential; the live in-cluster read stays 5x0.1.
//! Each shape is a Kubernetes `Secret` manifest rendered as pure JSON, which
//! `kubectl apply -f` accepts.

use serde_json::{Value, json};
use wamn_control_registry::Triple;

use crate::name::{
    APP_ROLE, cdc_object_name, project_env_cdc_secret_name, project_env_secret_name,
    workload_secret_name,
};
use crate::session_target::{SESSION_TARGET_KEY, SessionTarget};
use crate::workload_role::{WorkloadRoleFamily, WorkloadSecretBodyKind};

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
/// Three shapes, not one variant per family. Which shape a family
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
    /// The checked audience binding and reader credential in `target.json`.
    SessionTarget(&'a SessionTarget),
}

impl WorkloadSecretBody<'_> {
    fn kind(self) -> WorkloadSecretBodyKind {
        match self {
            Self::Url(_) => WorkloadSecretBodyKind::Url,
            Self::TenantUrl { .. } => WorkloadSecretBodyKind::TenantUrl,
            Self::SessionTarget(_) => WorkloadSecretBodyKind::SessionTarget,
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
        WorkloadSecretBody::SessionTarget(target) => json!({
            (SESSION_TARGET_KEY): target.to_json().expect("validated session target serializes"),
        }),
    };
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": metadata,
        "type": "Opaque",
        "stringData": string_data,
    })
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

    #[test]
    fn session_reader_secret_round_trips_the_checked_target() {
        let triple = Triple::new("acme", "receiving", "dev");
        let database = crate::project_env_database_name("acme", "receiving", "dev", "k3m9x2p7");
        let role = crate::workload_generation_role(
            WorkloadRoleFamily::SessionRoleReader,
            crate::WorkloadRoleScope::ProjectEnvironment {
                org: "acme",
                project: "receiving",
                environment: "dev",
                database: &database,
            },
            crate::CredentialGeneration::A,
        )
        .unwrap();
        let url = format!("postgres://{role}:fixture-password@database.invalid/{database}");
        let target = SessionTarget::new(&triple, "k3m9x2p7", "t1", &url).unwrap();
        let secret = render_workload_secret_manifest(
            WorkloadRoleFamily::SessionRoleReader,
            &triple,
            "wamn-system",
            WorkloadSecretBody::SessionTarget(&target),
        );
        let data = secret["stringData"].as_object().unwrap();
        assert_eq!(data.len(), 1);
        let parsed =
            SessionTarget::from_json(data[SESSION_TARGET_KEY].as_str().unwrap().as_bytes())
                .unwrap();
        assert_eq!(
            parsed.audience(),
            "urn:wamn:project-env:acme:receiving:dev:k3m9x2p7"
        );
        assert_eq!(parsed.tenant_id(), "t1");
        assert_eq!(parsed.connection().url(), url);
        assert!(
            !format!("{:?}", WorkloadSecretBody::SessionTarget(&target))
                .contains("fixture-password")
        );
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
}
