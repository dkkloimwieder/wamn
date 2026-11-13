//! Pure target-document tests; registry freshness and database grants are live tests.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use wamn_control_provision::session_target::SessionTarget;
use wamn_control_provision::workload_role::{WorkloadRoleIdentity, workload_generation_role};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily, project_env_database_name};
use wamn_registry::Triple;

const INSTANCE: &str = "k3m9x2p7";
const PASSWORD: &str = "fixture-secret-do-not-log";

fn triple(org: &str, env: &str) -> Triple {
    Triple {
        org: org.into(),
        project: "widgets".into(),
        env: env.into(),
    }
}

fn credential(
    triple: &Triple,
    instance: &str,
    family: WorkloadRoleFamily,
    generation: CredentialGeneration,
) -> String {
    let database =
        project_env_database_name(&triple.org, &triple.project, triple.env.as_str(), instance);
    let role = workload_generation_role(
        family,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: &triple.org,
            project: &triple.project,
            environment: triple.env.as_str(),
            database: &database,
        },
        generation,
    )
    .expect("fixture family uses environment target");
    format!("postgres://{role}:{PASSWORD}@database.invalid/{database}")
}

fn target(triple: &Triple, instance: &str, generation: CredentialGeneration) -> SessionTarget {
    let url = credential(
        triple,
        instance,
        WorkloadRoleFamily::SessionRoleReader,
        generation,
    );
    SessionTarget::new(
        triple,
        instance,
        &wamn_registry::project_env_key(&triple.org, &triple.project, triple.env.as_str()),
        &url,
    )
    .expect("valid provisioned target")
}

fn document() -> Value {
    serde_json::from_str(
        &target(&triple("acme", "dev"), INSTANCE, CredentialGeneration::A)
            .to_json()
            .unwrap(),
    )
    .unwrap()
}

fn refused(document: &Value) {
    let bytes = serde_json::to_vec(document).unwrap();
    let error =
        SessionTarget::from_json(&bytes).expect_err("forged or malformed target must refuse");
    let diagnostics = format!("{error:?} {error}");
    assert!(
        !diagnostics.contains(PASSWORD),
        "target refusal must redact credentials"
    );
    assert!(
        !diagnostics.contains("postgres://"),
        "target refusal must not echo a URL"
    );
}

#[test]
fn audience_spelling_and_each_environment_instance_are_exact() {
    let mut audiences = BTreeSet::new();
    let mut databases = BTreeSet::new();
    let mut logins = BTreeSet::new();
    for org in ["acme", "globex"] {
        for env in ["dev", "prod"] {
            for instance in [INSTANCE, "z9z9z9z9"] {
                let coordinates = triple(org, env);
                let a = target(&coordinates, instance, CredentialGeneration::A);
                let b = target(&coordinates, instance, CredentialGeneration::B);
                assert_eq!(
                    b.audience(),
                    a.audience(),
                    "credential rotation keeps the audience"
                );
                assert_eq!(a.triple(), &coordinates);
                assert_eq!(a.instance_suffix(), instance);
                assert_eq!(
                    a.project_env_key(),
                    wamn_registry::project_env_key(
                        &coordinates.org,
                        &coordinates.project,
                        coordinates.env.as_str()
                    )
                );
                assert_eq!(a.connection().generation(), CredentialGeneration::A);
                assert_eq!(b.connection().generation(), CredentialGeneration::B);
                assert!(audiences.insert(a.audience().to_owned()));
                assert!(databases.insert(a.connection().database().to_owned()));
                assert!(logins.insert(a.connection().role().to_owned()));
                assert!(logins.insert(b.connection().role().to_owned()));
            }
        }
    }
    assert_eq!((audiences.len(), databases.len(), logins.len()), (8, 8, 16));
}

#[test]
fn credential_target_round_trips_both_generations() {
    for generation in [CredentialGeneration::A, CredentialGeneration::B] {
        let coordinates = triple("acme", "dev");
        let url = credential(
            &coordinates,
            INSTANCE,
            WorkloadRoleFamily::SessionRoleReader,
            generation,
        );
        let original = SessionTarget::new(
            &coordinates,
            INSTANCE,
            &wamn_registry::project_env_key(
                &coordinates.org,
                &coordinates.project,
                coordinates.env.as_str(),
            ),
            &url,
        )
        .unwrap();
        let encoded = original.to_json().unwrap();
        let decoded = SessionTarget::from_json(encoded.as_bytes()).unwrap();
        assert_eq!(decoded.triple(), &coordinates);
        assert_eq!(decoded.audience(), original.audience());
        assert_eq!(decoded.instance_suffix(), INSTANCE);
        assert_eq!(decoded.project_env_key(), original.project_env_key());
        assert_eq!(decoded.connection().url(), url);
        assert_eq!(decoded.connection().generation(), generation);
    }
}

#[test]
fn malformed_missing_duplicate_and_extra_fields_refuse() {
    for bytes in [b"".as_slice(), b"{", b"null", b"[]", b"\xff"] {
        assert!(SessionTarget::from_json(bytes).is_err());
    }
    let valid = document();
    let encoded = serde_json::to_string(&valid).unwrap();
    assert!(SessionTarget::from_json(format!("{encoded} true").as_bytes()).is_err());
    for (key, value) in valid.as_object().unwrap() {
        let mut absent = valid.clone();
        absent.as_object_mut().unwrap().remove(key);
        refused(&absent);
        let mut wrong_type = valid.clone();
        wrong_type[key] = Value::Null;
        refused(&wrong_type);
        // Preserve duplicate JSON keys; parsing into Value first would erase them.
        let duplicate = format!(
            "{},{}:{}}}",
            encoded.strip_suffix('}').unwrap(),
            serde_json::to_string(key).unwrap(),
            value
        );
        let error = SessionTarget::from_json(duplicate.as_bytes())
            .expect_err("duplicate target field must refuse even when values agree");
        assert!(!format!("{error:?} {error}").contains(PASSWORD));
    }
    let mut extra = valid;
    extra["organization_override"] = json!("globex");
    refused(&extra);
}

#[test]
fn forged_audience_database_coordinates_and_incarnation_refuse() {
    for (field, value) in [
        ("audience", "widgets"),
        (
            "audience",
            "urn:wamn:project-env:globex:widgets:dev:k3m9x2p7",
        ),
        (
            "audience",
            "urn:wamn:project-env:acme:widgets:dev:k3m9x2p7/",
        ),
        ("database", "wamn_system"),
        ("database", "wamn-db-acme--widgets--prod--k3m9x2p7"),
        ("org", "globex"),
        ("project", "other"),
        ("env", "prod"),
        ("instance_suffix", "z9z9z9z9"),
    ] {
        let mut forged = document();
        forged[field] = json!(value);
        refused(&forged);
    }
    let old = target(&triple("acme", "dev"), INSTANCE, CredentialGeneration::A);
    let replacement = target(&triple("acme", "dev"), "z9z9z9z9", CredentialGeneration::A);
    let mut forged: Value = serde_json::from_str(&replacement.to_json().unwrap()).unwrap();
    forged["database_url"] = json!(old.connection().url());
    refused(&forged);
    let mut old_document: Value = serde_json::from_str(&old.to_json().unwrap()).unwrap();
    old_document["database_url"] = json!(replacement.connection().url());
    refused(&old_document);
}

#[test]
fn invalid_coordinates_and_instance_spelling_refuse_before_use() {
    for (field, value) in [
        ("org", ""),
        ("org", "acme:other"),
        ("org", "Acme"),
        ("project", "widgets--other"),
        ("project", "wamn-system"),
        ("env", "dev/prod"),
        ("env", ""),
        ("instance_suffix", "short"),
        ("instance_suffix", "K3m9x2p7"),
        ("instance_suffix", "k3m9x2p70"),
        ("instance_suffix", "k3m9x2p-"),
    ] {
        let mut invalid = document();
        invalid[field] = json!(value);
        refused(&invalid);
    }
}

#[test]
fn wrong_reader_families_and_url_overrides_refuse() {
    let coordinates = triple("acme", "dev");
    for family in [
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleFamily::ServiceReader,
        WorkloadRoleFamily::ExecutorPlatform,
    ] {
        let url = credential(&coordinates, INSTANCE, family, CredentialGeneration::A);
        let mut forged = document();
        forged["database_url"] = json!(url);
        refused(&forged);
    }
    let raw = credential(
        &coordinates,
        INSTANCE,
        WorkloadRoleFamily::SessionRoleReader,
        CredentialGeneration::A,
    );
    for url in [
        format!("{raw}?options=-crole=postgres"),
        format!("{raw}#override"),
        raw.replace("/wamn-db-acme--dev--widgets--k3m9x2p7", "/wamn_system"),
    ] {
        let mut forged = document();
        forged["database_url"] = json!(url);
        refused(&forged);
    }
}

#[test]
fn malformed_and_mismatched_project_coordinates_refuse() {
    let coordinates = triple("acme", "dev");
    let url = credential(
        &coordinates,
        INSTANCE,
        WorkloadRoleFamily::SessionRoleReader,
        CredentialGeneration::A,
    );
    for project_env_key in [
        "t1".to_owned(),
        "globex--dev--widgets".to_owned(),
        String::new(),
        "a".repeat(65),
        "t.1".to_owned(),
        "t 1".to_owned(),
        "t/1".to_owned(),
        "t'1".to_owned(),
        "é".to_owned(),
    ] {
        assert!(SessionTarget::new(&coordinates, INSTANCE, &project_env_key, &url).is_err());
        let mut invalid = document();
        invalid["project_env_key"] = json!(project_env_key);
        refused(&invalid);
    }
}

#[test]
fn target_clone_and_all_refusal_diagnostics_redact_credentials() {
    let target = target(&triple("acme", "dev"), INSTANCE, CredentialGeneration::A);
    let cloned = target.clone();
    assert_eq!(cloned.triple(), target.triple());
    for diagnostic in [
        format!("{target:?}"),
        format!("{cloned:?}"),
        format!("{:?}", target.connection()),
        format!("{}", target.connection()),
    ] {
        assert!(!diagnostic.contains(PASSWORD));
        assert!(!diagnostic.contains("database.invalid"));
        assert!(!diagnostic.contains("postgres://"));
    }
    for field in [
        "audience",
        "org",
        "project",
        "env",
        "instance_suffix",
        "project_env_key",
        "database",
        "database_url",
    ] {
        let mut invalid = document();
        invalid[field] = json!(format!("postgres://{PASSWORD}@bad.invalid"));
        refused(&invalid);
    }
}
