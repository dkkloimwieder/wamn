//! Integration tests for the control-plane registry model: import/export
//! round-trip, triple-driven routing, and placement resolution (cluster + Secret
//! reference), including the D18 recovery-domain cluster derivation and the
//! wamn-8df.4 org-scoped policies (templates; T2/T4 coexistence).

use wamn_control_registry::{
    ClusterRef, EventReader, Org, Project, ProjectEnv, RecoveryDomain, Registry, RegistryError,
    SecretRef, Template, Triple,
};

/// A registry with a dedicated + a pooled org, each stamped from the `standard`
/// template (dev/prod own + canary sharing prod's recovery domain) with a project
/// provisioned across all three envs.
fn sample() -> Registry {
    let mut env_policies = Vec::new();
    for org in ["acme", "try"] {
        env_policies.extend(Template::standard().stamp(org, "wamn-pg").1);
    }

    // Each env carries the instance suffix provisioning minted for it — 8 bytes of
    // `[a-z0-9]`, the part of a derived physical name the triple cannot supply.
    let envs = [
        ("dev", "k3m9x2p7"),
        ("prod", "q80zdw41"),
        ("canary", "0z9a8b7c"),
    ];
    let mut project_envs = Vec::new();
    for (org, project, secret_prefix) in [("acme", "billing", "acme"), ("try", "demo", "try")] {
        for (env, instance) in envs {
            project_envs.push(ProjectEnv {
                triple: Triple::new(org, project, env),
                db_secret: SecretRef::new(format!("wamn-db-{secret_prefix}-{env}")),
                instance_suffix: instance.into(),
            });
        }
    }

    Registry {
        schema_version: "0.1".into(),
        env_policies,
        orgs: vec![Org::dedicated("acme"), Org::pooled("try", "wamn-pg")],
        projects: vec![
            Project {
                org: "acme".into(),
                id: "billing".into(),
            },
            Project {
                org: "try".into(),
                id: "demo".into(),
            },
        ],
        project_envs,
    }
}

#[test]
fn sample_is_valid() {
    let r = sample();
    assert!(r.is_valid(), "issues: {:?}", r.issues());
}

#[test]
fn json_round_trip_is_structurally_stable() {
    let r = sample();
    let json = r.to_json();
    let back = Registry::from_json(&json).expect("parses");
    assert_eq!(r, back);
    // Kebab-case wire keys (the house JSON style).
    assert!(json.contains("\"schema-version\""));
    assert!(json.contains("\"env-policies\""));
    assert!(json.contains("\"project-envs\""));
    assert!(json.contains("\"db-secret\""));
    // The instance identity is on the wire (wamn-0h0g.15.89) — a consumer that
    // derives the environment namespace reads it from the exported document.
    assert!(json.contains("\"instance-suffix\": \"k3m9x2p7\""));
    // env serializes as a bare lowercase string; placement is a tagged object.
    assert!(json.contains("\"env\": \"prod\""));
    assert!(json.contains("\"kind\": \"dedicated\""));
    assert!(json.contains("\"kind\": \"pooled\""));
    // recovery-domain shared-with is the {"shared-with": ...} shape.
    assert!(json.contains("\"shared-with\": \"prod\""));
}

#[test]
fn minimal_registry_round_trips_minimally() {
    // Default-empty collections are omitted on export.
    let json = Registry::empty().to_json();
    assert!(!json.contains("orgs"));
    assert!(!json.contains("env-policies"));
    let back = Registry::from_json(&json).expect("parses");
    assert_eq!(back, Registry::empty());
}

/// The instance suffix is a REQUIRED wire field and the export contract is closed
/// in BOTH directions (wamn-0h0g.15.89).
///
/// * **Missing** — a document exported before the field existed carries no
///   `instance-suffix`, and with no `serde(default)` it FAILS to parse. That is
///   the intended ruling: the storage column is `NOT NULL` under a
///   `^[a-z0-9]{8}$` CHECK, so an absent suffix is not an older-but-usable row
///   but a project-env with NO instance identity, and a defaulted empty string
///   would derive the namespace `wamn-acme--billing--prod--`, which is not a
///   DNS-1123 label. Refusing loudly beats manufacturing a handle that addresses
///   the wrong resources.
/// * **Unknown** — `deny_unknown_fields` is intact, so a field a NEWER writer
///   adds is refused rather than silently dropped. That is the direction this
///   field addition itself breaks, and `SCHEMA_VERSION` cannot signal it (the
///   `0.1` literal is pinned by `deploy/sql/system-schema.sql` and a frozen
///   conformance literal), so the parse failure is the entire signal.
#[test]
fn the_instance_suffix_is_a_required_wire_field_in_both_directions() {
    let pe = ProjectEnv {
        triple: Triple::new("acme", "billing", "prod"),
        db_secret: SecretRef::new("wamn-db-acme--billing--prod"),
        instance_suffix: "k3m9x2p7".into(),
    };
    let json = serde_json::to_string(&pe).expect("serializes");
    assert!(json.contains("\"instance-suffix\":\"k3m9x2p7\""));
    assert_eq!(
        serde_json::from_str::<ProjectEnv>(&json).expect("parses"),
        pe
    );

    // A pre-field document: refused, and the refusal names the missing field.
    let older = r#"{"triple":{"org":"acme","project":"billing","env":"prod"},
        "db-secret":{"name":"wamn-db-acme--billing--prod"}}"#;
    let missing = serde_json::from_str::<ProjectEnv>(older)
        .expect_err("an absent instance suffix must not default");
    assert!(
        missing.to_string().contains("instance-suffix"),
        "the refusal must name the missing field: {missing}"
    );
    // …and it is refused through the whole-registry import path too, for the same
    // named reason (nothing upstream defaults it in).
    let doc = format!("{{\"schema-version\":\"0.1\",\"project-envs\":[{older}]}}");
    let imported = Registry::from_json(&doc).expect_err("the import path refuses it too");
    assert!(
        imported.to_string().contains("instance-suffix"),
        "the import refusal must name the missing field: {imported}"
    );

    // deny_unknown_fields is intact: an extra field is refused, not ignored.
    let fat = json.replace(
        "\"instance-suffix\":\"k3m9x2p7\"",
        "\"instance-suffix\":\"k3m9x2p7\",\"retired-suffix\":\"q80zdw41\"",
    );
    assert!(serde_json::from_str::<ProjectEnv>(&fat).is_err());
}

#[test]
fn resolve_routes_each_env_to_the_derived_cluster() {
    let r = sample();

    // Dedicated org: dev(own) → <org>-dev, prod(own) → <org>-prod, canary sharing
    // prod's recovery domain → <org>-prod (the T2 collapse, now a policy field).
    let prod = r
        .resolve(&Triple::new("acme", "billing", "prod"))
        .expect("resolves");
    assert_eq!(prod.cluster, ClusterRef::new("acme-prod"));
    assert_eq!(prod.secret, SecretRef::new("wamn-db-acme-prod"));

    let canary = r
        .resolve(&Triple::new("acme", "billing", "canary"))
        .expect("resolves");
    assert_eq!(
        canary.cluster,
        ClusterRef::new("acme-prod"),
        "canary shares prod's recovery domain"
    );

    let dev = r
        .resolve(&Triple::new("acme", "billing", "dev"))
        .expect("resolves");
    assert_eq!(
        dev.cluster,
        ClusterRef::new("acme-dev"),
        "dev has its own recovery domain"
    );
    assert_eq!(dev.secret, SecretRef::new("wamn-db-acme-dev"));
}

#[test]
fn resolve_routes_canary_own_to_its_own_cluster() {
    // The T4 property: with canary as its OWN recovery domain (a policy field, not
    // a stored canary_cluster + special resolver), a dedicated org's canary
    // resolves to its own cluster — distinct from prod.
    let mut r = sample();
    for p in &mut r.env_policies {
        if p.org == "acme" && p.policy.name == "canary" {
            p.policy.recovery_domain = RecoveryDomain::Own;
        }
    }
    let canary = r
        .resolve(&Triple::new("acme", "billing", "canary"))
        .expect("resolves");
    assert_eq!(canary.cluster, ClusterRef::new("acme-canary"));
    let prod = r
        .resolve(&Triple::new("acme", "billing", "prod"))
        .expect("resolves");
    assert_ne!(
        canary.cluster, prod.cluster,
        "an own-domain canary is not prod's cluster"
    );
}

#[test]
fn t2_and_t4_orgs_coexist_via_org_scoped_policies() {
    // THE wamn-8df.4 headline: one platform holds a `standard` org (canary
    // shared-with prod) AND a `dedicated` org (canary own) at the same time —
    // impossible under platform-global policies, where one canary row would have
    // forced the same shape on every dedicated org.
    let mut env_policies = Template::standard().stamp("acme", "wamn-pg").1;
    env_policies.extend(Template::dedicated().stamp("bigco", "wamn-pg").1);
    let projects = vec![
        Project {
            org: "acme".into(),
            id: "billing".into(),
        },
        Project {
            org: "bigco".into(),
            id: "ledger".into(),
        },
    ];
    let project_envs = vec![
        ProjectEnv {
            triple: Triple::new("acme", "billing", "canary"),
            db_secret: SecretRef::new("wamn-db-acme-canary"),
            instance_suffix: "k3m9x2p7".into(),
        },
        ProjectEnv {
            triple: Triple::new("bigco", "ledger", "canary"),
            db_secret: SecretRef::new("wamn-db-bigco-canary"),
            instance_suffix: "q80zdw41".into(),
        },
    ];
    let r = Registry {
        schema_version: "0.1".into(),
        env_policies,
        orgs: vec![Org::dedicated("acme"), Org::dedicated("bigco")],
        projects,
        project_envs,
    };
    assert!(r.is_valid(), "issues: {:?}", r.issues());

    // The SAME env slug resolves to a different physical shape per org.
    let acme = r
        .resolve(&Triple::new("acme", "billing", "canary"))
        .expect("resolves");
    assert_eq!(
        acme.cluster,
        ClusterRef::new("acme-prod"),
        "standard: canary co-resides in prod's recovery domain (T2)"
    );
    let bigco = r
        .resolve(&Triple::new("bigco", "ledger", "canary"))
        .expect("resolves");
    assert_eq!(
        bigco.cluster,
        ClusterRef::new("bigco-canary"),
        "dedicated: canary owns its recovery domain (T4)"
    );
}

#[test]
fn resolve_collapses_a_pooled_org_onto_the_pool() {
    let r = sample();
    for env in ["dev", "prod", "canary"] {
        let res = r
            .resolve(&Triple::new("try", "demo", env))
            .expect("resolves");
        assert_eq!(
            res.cluster,
            ClusterRef::new("wamn-pg"),
            "every pooled env resolves to the shared pool"
        );
    }
}

#[test]
fn resolve_reports_each_missing_level() {
    let r = sample();
    assert_eq!(
        r.resolve(&Triple::new("ghost", "billing", "prod")),
        Err(RegistryError::UnknownOrg("ghost".into()))
    );
    assert_eq!(
        r.resolve(&Triple::new("acme", "ghost", "prod")),
        Err(RegistryError::UnknownProject {
            org: "acme".into(),
            project: "ghost".into(),
        })
    );
    // Org + project exist, but this env was never provisioned (drop it).
    let mut r = sample();
    r.project_envs
        .retain(|pe| pe.triple != Triple::new("acme", "billing", "canary"));
    assert_eq!(
        r.resolve(&Triple::new("acme", "billing", "canary")),
        Err(RegistryError::UnknownProjectEnv(Triple::new(
            "acme", "billing", "canary"
        )))
    );

    // A provisioned project-env whose env names no policy → UnknownEnvPolicy (the
    // cluster cannot be derived; validate() flags this as `unknown-env`).
    let mut r = sample();
    r.project_envs.push(ProjectEnv {
        triple: Triple::new("acme", "billing", "ghostenv"),
        db_secret: SecretRef::new("wamn-db-acme-ghost"),
        instance_suffix: "0z9a8b7c".into(),
    });
    assert_eq!(
        r.resolve(&Triple::new("acme", "billing", "ghostenv")),
        Err(RegistryError::UnknownEnvPolicy("ghostenv".into()))
    );
}

#[test]
fn triple_host_label_is_derived_not_parsed() {
    let t = Triple::new("acme", "billing", "prod");
    assert_eq!(t.host_label(), "billing--prod.acme");
    assert_eq!(t.to_string(), "acme/billing/prod");
}

/// The CDC reader registration (wamn-l5i9.9) round-trips on the kebab-case
/// wire, and its Secret field is a REFERENCE ([`SecretRef`]) — the row model
/// the reader service (l5i9.10) deserializes.
#[test]
fn event_reader_registration_round_trips_with_a_secret_reference() {
    let r = EventReader {
        triple: Triple::new("acme", "billing", "dev"),
        publication: "wamn_cdc_acme__billing__dev".into(),
        slot: "wamn_cdc_acme__billing__dev".into(),
        stream: "EVT_acme_dev".into(),
        replication_secret: SecretRef::new("wamn-cdc-acme--billing--dev"),
        enabled: true,
    };
    let json = serde_json::to_string_pretty(&r).expect("serializes");
    let back: EventReader = serde_json::from_str(&json).expect("parses");
    assert_eq!(r, back);
    // Kebab-case wire keys; the credential travels as a reference, never material.
    assert!(json.contains("\"replication-secret\""));
    assert!(json.contains("\"wamn-cdc-acme--billing--dev\""));
    assert!(!json.to_lowercase().contains("password"));
    // An unknown field is rejected (deny_unknown_fields — a fat row with a
    // smuggled credential column fails to parse).
    let bad = json.replace(
        "\"enabled\": true",
        "\"enabled\": true, \"url\": \"postgres://…\"",
    );
    assert!(serde_json::from_str::<EventReader>(&bad).is_err());
}
