//! Integration tests for the control-plane registry model: import/export
//! round-trip, triple-driven routing, and placement resolution (cluster + Secret
//! reference), including the D18 recovery-domain cluster derivation and the
//! wamn-8df.4 org-scoped policies (templates; T2/T4 coexistence).

use wamn_registry::{
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

    let mut project_envs = Vec::new();
    for (org, project, secret_prefix) in [("acme", "billing", "acme"), ("try", "demo", "try")] {
        for env in ["dev", "prod", "canary"] {
            project_envs.push(ProjectEnv {
                triple: Triple::new(org, project, env),
                db_secret: SecretRef::new(format!("wamn-db-{secret_prefix}-{env}")),
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
        },
        ProjectEnv {
            triple: Triple::new("bigco", "ledger", "canary"),
            db_secret: SecretRef::new("wamn-db-bigco-canary"),
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
