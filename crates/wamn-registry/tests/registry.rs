//! Integration tests for the control-plane registry model: import/export
//! round-trip, triple-driven routing, and placement resolution (tier + cluster
//! + Secret reference), including the recovery-domain cluster split.

use wamn_registry::{
    ClusterRef, Env, Org, Project, ProjectEnv, Registry, RegistryError, SecretRef, Side, Tier,
    Triple,
};

/// A registry with a T2 (standard), a T3 (trials), and a T4 (dedicated) org,
/// each with a project provisioned across all three envs.
fn sample() -> Registry {
    let mut project_envs = Vec::new();
    for (org, project, secret_prefix) in [
        ("acme", "billing", "acme"),
        ("try", "demo", "try"),
        ("ded", "app", "ded"),
    ] {
        for env in Env::ALL {
            project_envs.push(ProjectEnv {
                triple: Triple::new(org, project, env),
                db_secret: SecretRef::new(format!("wamn-db-{secret_prefix}-{env}")),
            });
        }
    }
    Registry {
        schema_version: "0.1".into(),
        orgs: vec![
            Org {
                id: "acme".into(),
                tier: Tier::Standard,
                prod_cluster: ClusterRef::new("acme-prod"),
                canary_cluster: None,
                dev_cluster: ClusterRef::new("acme-dev"),
            },
            Org {
                id: "try".into(),
                tier: Tier::Trials,
                // A trials org's prod and dev both live on the shared pool.
                prod_cluster: ClusterRef::new("wamn-pg"),
                canary_cluster: None,
                dev_cluster: ClusterRef::new("wamn-pg"),
            },
            Org {
                id: "ded".into(),
                tier: Tier::Dedicated,
                // A dedicated (T4) org gives canary its OWN cluster (wamn-q3n.14).
                prod_cluster: ClusterRef::new("ded-prod"),
                canary_cluster: Some(ClusterRef::new("ded-canary")),
                dev_cluster: ClusterRef::new("ded-dev"),
            },
        ],
        projects: vec![
            Project {
                org: "acme".into(),
                id: "billing".into(),
            },
            Project {
                org: "try".into(),
                id: "demo".into(),
            },
            Project {
                org: "ded".into(),
                id: "app".into(),
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
    assert!(json.contains("\"project-envs\""));
    assert!(json.contains("\"prod-cluster\""));
    assert!(json.contains("\"db-secret\""));
    // Env serializes as a bare lowercase string.
    assert!(json.contains("\"env\": \"prod\""));
}

#[test]
fn minimal_registry_round_trips_minimally() {
    // Default-empty collections are omitted on export.
    let json = Registry::empty().to_json();
    assert!(!json.contains("orgs"));
    assert!(!json.contains("projects"));
    let back = Registry::from_json(&json).expect("parses");
    assert_eq!(back, Registry::empty());
}

#[test]
fn env_side_maps_canary_and_prod_to_prod_dev_to_dev() {
    assert_eq!(Env::Prod.side(), Side::Prod);
    assert_eq!(Env::Canary.side(), Side::Prod);
    assert_eq!(Env::Dev.side(), Side::Dev);
}

#[test]
fn resolve_routes_each_env_to_the_correct_cluster() {
    let r = sample();

    // Standard org: prod + canary -> the prod cluster; dev -> the dev cluster.
    let prod = r
        .resolve(&Triple::new("acme", "billing", Env::Prod))
        .expect("resolves");
    assert_eq!(prod.tier, Tier::Standard);
    assert_eq!(prod.cluster, ClusterRef::new("acme-prod"));
    assert_eq!(prod.secret, SecretRef::new("wamn-db-acme-prod"));

    let canary = r
        .resolve(&Triple::new("acme", "billing", Env::Canary))
        .expect("resolves");
    assert_eq!(
        canary.cluster,
        ClusterRef::new("acme-prod"),
        "canary shares prod's recovery domain"
    );

    let dev = r
        .resolve(&Triple::new("acme", "billing", Env::Dev))
        .expect("resolves");
    assert_eq!(
        dev.cluster,
        ClusterRef::new("acme-dev"),
        "dev has its own recovery domain"
    );
    assert_eq!(dev.secret, SecretRef::new("wamn-db-acme-dev"));
}

#[test]
fn resolve_routes_dedicated_canary_to_its_own_cluster() {
    let r = sample();
    // A dedicated (T4) org: canary resolves to its OWN cluster (its own recovery
    // domain / independent PITR), distinct from prod — the §T4 property that the
    // T2 Env::side collapse cannot express (wamn-q3n.14).
    let canary = r
        .resolve(&Triple::new("ded", "app", Env::Canary))
        .expect("resolves");
    assert_eq!(canary.tier, Tier::Dedicated);
    assert_eq!(canary.cluster, ClusterRef::new("ded-canary"));

    let prod = r
        .resolve(&Triple::new("ded", "app", Env::Prod))
        .expect("resolves");
    assert_eq!(prod.cluster, ClusterRef::new("ded-prod"));

    let dev = r
        .resolve(&Triple::new("ded", "app", Env::Dev))
        .expect("resolves");
    assert_eq!(dev.cluster, ClusterRef::new("ded-dev"));

    assert_ne!(
        canary.cluster, prod.cluster,
        "a dedicated org's canary is its own recovery domain, not prod's"
    );
}

#[test]
fn resolve_collapses_a_trials_org_onto_the_pool() {
    let r = sample();
    for env in Env::ALL {
        let res = r
            .resolve(&Triple::new("try", "demo", env))
            .expect("resolves");
        assert_eq!(res.tier, Tier::Trials);
        assert_eq!(
            res.cluster,
            ClusterRef::new("wamn-pg"),
            "every trials env resolves to the shared pool"
        );
    }
}

#[test]
fn resolve_reports_each_missing_level() {
    let r = sample();
    assert_eq!(
        r.resolve(&Triple::new("ghost", "billing", Env::Prod)),
        Err(RegistryError::UnknownOrg("ghost".into()))
    );
    assert_eq!(
        r.resolve(&Triple::new("acme", "ghost", Env::Prod)),
        Err(RegistryError::UnknownProject {
            org: "acme".into(),
            project: "ghost".into(),
        })
    );
    // Org + project exist, but this env was never provisioned (drop it).
    let mut r = sample();
    r.project_envs
        .retain(|pe| pe.triple != Triple::new("acme", "billing", Env::Canary));
    assert_eq!(
        r.resolve(&Triple::new("acme", "billing", Env::Canary)),
        Err(RegistryError::UnknownProjectEnv(Triple::new(
            "acme",
            "billing",
            Env::Canary
        )))
    );
}

#[test]
fn triple_host_label_is_derived_not_parsed() {
    let t = Triple::new("acme", "billing", Env::Prod);
    assert_eq!(t.host_label(), "billing--prod.acme");
    assert_eq!(t.to_string(), "acme/billing/prod");
}
