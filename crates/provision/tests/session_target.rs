//! Public target documents bind registry coordinates to one reader login.
use serde_json::json;
use wamn_control_provision::session_target::SessionTarget;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_postgres::{ConnectionConfig, SecretRef};
use wamn_registry::Triple;

#[test]
fn changed_target_identity_is_refused() {
    fn target() -> SessionTarget {
        let triple = Triple::new("acme", "dev".parse().unwrap(), "widgets");
        let database = wamn_control_provision::project_env_database_name(
            "acme",
            "widgets",
            &"dev".parse().unwrap(),
            &"k3m9x2p7".parse().unwrap(),
        );
        let username = workload_login(
            WorkloadRoleFamily::SessionRoleReader,
            WorkloadRoleIdentity::ProjectEnvironment {
                org: "acme",
                project: "widgets",
                environment: &"dev".parse().unwrap(),
                database: &database,
            },
        )
        .unwrap();
        let connection = ConnectionConfig {
            host: "database.invalid".into(),
            port: 5432,
            database,
            username,
            credentials: SecretRef {
                namespace: "acme".into(),
                name: "reader".into(),
            },
            server_ca: SecretRef {
                namespace: "acme".into(),
                name: "ca".into(),
            },
        };
        SessionTarget::new(
            &triple,
            &"k3m9x2p7"
                .parse::<wamn_registry::instance_suffix::InstanceSuffix>()
                .unwrap(),
            &wamn_registry::project_env_key("acme", "widgets", &"dev".parse().unwrap()),
            &connection.to_json().unwrap(),
        )
        .unwrap()
    }

    let source: serde_json::Value = serde_json::from_str(&target().to_json().unwrap()).unwrap();
    for path in [
        "/audience",
        "/org",
        "/project",
        "/env",
        "/connection/database",
        "/connection/username",
    ] {
        let mut changed = source.clone();
        *changed.pointer_mut(path).unwrap() = json!("wrong");
        assert!(SessionTarget::from_json(&serde_json::to_vec(&changed).unwrap()).is_err());
    }
    let mut changed = source;
    changed["database_url"] = json!("postgres://reader:password@database.invalid/database");
    assert!(SessionTarget::from_json(&serde_json::to_vec(&changed).unwrap()).is_err());
}
