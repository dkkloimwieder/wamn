//! System bootstrap requests and redacted Kubernetes credential decoding.

use std::path::Path;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::Value;
use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_ROLE, identity_issuer_generation_role,
};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::document::ClusterDocument;
use super::job::{self, JobRequest, JobSecret};
use crate::kubectl;

pub(super) const DATABASE_HOST: &str = "wamn-pg-rw.platform.svc.cluster.local";

/// Encoded Secret data. Debug output omits every value.
#[derive(Deserialize)]
pub struct SecretData {
    #[serde(default)]
    data: std::collections::BTreeMap<String, String>,
}

impl std::fmt::Debug for SecretData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretData")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

pub(super) fn secret_text(secret: &SecretData, key: &str) -> anyhow::Result<String> {
    let encoded = secret
        .data
        .get(key)
        .with_context(|| format!("the Secret has no {key}"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("the Secret key {key} is not base64"))?;
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("the Secret key {key} is not UTF-8"))
}

/// Construct the system administrator credential from the CNPG Secret.
pub fn administrator(secret: &SecretData) -> anyhow::Result<JobSecret> {
    let mut url = url::Url::parse(&format!(
        "postgresql://postgres@{DATABASE_HOST}/wamn_system"
    ))?;
    url.set_password(Some(&secret_text(secret, "password")?.replace('%', "%25")))
        .map_err(|()| anyhow::anyhow!("encode the system administrator password"))?;
    Ok(JobSecret {
        name: "wamn-system-admin".into(),
        key: "url".into(),
        value: url.into(),
        environment: "WAMN_SYSTEM_ADMIN_URL".into(),
    })
}

/// Construct the system, initial organization, issuer, and control credential requests.
pub fn provisioning_jobs(document: &ClusterDocument, image: &str) -> [JobRequest; 4] {
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);
    [
        (
            "platform-system",
            vec!["provision-system", "--platform-domain", domain],
            false,
        ),
        (
            "platform-org",
            vec![
                "provision-org",
                "--org",
                &document.org,
                "--template",
                "trials",
                "--pool",
                "wamn-pg",
                "--admin",
                &document.owner_email,
            ],
            false,
        ),
        (
            "platform-identity-issuer",
            vec![
                "provision-identity-issuer",
                "--issuer",
                super::identity::ISSUER,
                "--prepare-generation",
                "a",
                "--namespace",
                "identity",
                "--emit-secret",
                "/out/secret.json",
                "--db-host",
                DATABASE_HOST,
            ],
            true,
        ),
        (
            "platform-control-credentials",
            vec![
                "provision-org",
                "--org",
                &document.org,
                "--template",
                "trials",
                "--pool",
                "wamn-pg",
                "--admin",
                &document.owner_email,
                "--prepare-control-generation",
                "a",
                "--namespace",
                "hosts",
                "--emit-control-secret",
                "/out/secret.json",
                "--db-host",
                DATABASE_HOST,
            ],
            true,
        ),
    ]
    .map(|(name, arguments, emits_secret)| JobRequest {
        name: name.into(),
        image: image.into(),
        arguments: arguments.into_iter().map(str::to_owned).collect(),
        calls_kubernetes: false,
        emits_secret,
        ..JobRequest::default()
    })
}

/// Initialize system SQL and the credentials of the initial platform services.
///
/// Public Job results remain in `directory/jobs`, including the system SQL result.
///
/// # Errors
/// Returns SQL, credential, or Kubernetes failures without resetting database state.
pub async fn initialize(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let target = Target {
        namespace: "platform".into(),
        ..target.clone()
    };
    let superuser: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-pg-superuser",
    )
    .await?
    .context("missing Secret platform/wamn-pg-superuser")?;
    let admin = administrator(&superuser)?;
    let [system, org, issuer, control] = provisioning_jobs(document, chart.image("ctl")?);
    job::run(&target, directory, &system, std::slice::from_ref(&admin)).await?;
    job::run(&target, directory, &org, std::slice::from_ref(&admin)).await?;

    // The selected CNPG image supplies psql; the ctl image supplies WAMN SQL.
    let objects = super::manifests::objects(
        &chart.directory().join("platform/resources"),
        "cnpg-cluster.yaml",
        document,
    )?;
    let postgres = objects
        .iter()
        .find(|object| object["kind"] == "Cluster" && object["metadata"]["name"] == "wamn-pg")
        .and_then(|object| object["spec"]["imageName"].as_str())
        .context("selected CNPG cluster has no PostgreSQL image")?;
    let password = JobSecret {
        name: "wamn-bootstrap-probe".into(),
        key: "password".into(),
        value: secret_text(&superuser, "password")?,
        environment: "PGPASSWORD".into(),
    };
    for service in service_credentials(document, [issuer, control])? {
        credential(&target, directory, postgres, &admin, &password, &service).await?;
    }
    let administration = wamn_control_provision::render_control_administration_secret_manifest(
        &document.org,
        "hosts",
    );
    let name = administration["metadata"]["name"]
        .as_str()
        .context("control administration Secret name")?;
    let existing: Option<Value> =
        kubectl::get(&target.kubeconfig, &target.context, "hosts", "secret", name).await?;
    if existing.is_none() {
        // Create only: a later environment can add credentials to this Secret.
        kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            "hosts",
            &["create", "-f", "-"],
            Some(&serde_json::to_vec(&administration)?),
        )
        .await?;
    }
    Ok(())
}

struct Credential {
    request: JobRequest,
    namespace: &'static str,
    secret: String,
    roles: [String; 2],
    acl: &'static str,
    repair: String,
}

fn service_credentials(
    document: &ClusterDocument,
    [issuer, mut control]: [JobRequest; 2],
) -> anyhow::Result<[Credential; 2]> {
    let issuer_role =
        |generation| identity_issuer_generation_role(super::identity::ISSUER, generation);
    let control_role = |generation| {
        workload_generation_role(
            WorkloadRoleFamily::Control,
            WorkloadRoleScope::Org {
                org: &document.org,
                database: "wamn_system",
            },
            generation,
        )
    };
    control.name.push('-');
    control.name.push_str(&document.org);
    Ok([
        Credential {
            request: issuer,
            namespace: "identity",
            secret: "wamn-identity-db".into(),
            roles: [
                issuer_role(CredentialGeneration::A)?,
                issuer_role(CredentialGeneration::B)?,
            ],
            acl: IDENTITY_ISSUER_ROLE,
            repair: format!(
                "provision-identity-issuer --issuer {} --abort-generation",
                super::identity::ISSUER
            ),
        },
        Credential {
            request: control,
            namespace: "hosts",
            secret: format!("wamn-control-{}", document.org),
            roles: [
                control_role(CredentialGeneration::A)?,
                control_role(CredentialGeneration::B)?,
            ],
            acl: WorkloadRoleFamily::Control.acl_role(),
            repair: format!(
                "provision-org --org {} --template trials --pool wamn-pg --admin {} --abort-control-generation",
                document.org, document.owner_email
            ),
        },
    ])
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum GenerationState {
    Absent,
    Inactive,
    Active,
    Invalid,
}

/// Inspect real role attributes and direct authority, using the provisioner's catalog query.
fn generation_state_sql(role: &str, acl: &str) -> String {
    let role = wamn_pg_core::quote_literal(role);
    let acl = wamn_pg_core::quote_literal(acl);
    let state = sql::system_generation_state_sql().replace("$1", &role);
    let stable = sql::system_generation_state_sql().replace("$1", &acl);
    let grants = sql::role_database_grants_sql().replace("$1", &role);
    format!(
        "WITH state AS ({state}), stable AS ({stable}), grants AS ({grants}) \
         SELECT CASE WHEN NOT EXISTS (SELECT FROM state) THEN 'absent' \
         ELSE (SELECT CASE \
           WHEN NOT (rolsuper OR rolcreaterole OR rolcreatedb OR rolreplication OR rolbypassrls) \
             AND owned_objects = 0 AND rolinherit AND cardinality(member_roles) = 0 \
             AND membership_options_exact AND member_options_exact THEN \
             CASE WHEN NOT rolcanlogin AND NOT password_set \
                 AND valid_until = '1970-01-01T00:00:00Z' \
                 AND cardinality(memberships) = 0 AND cardinality(connect_databases) = 0 \
                 AND sessions = 0 AND NOT EXISTS (SELECT FROM grants) THEN 'inactive' \
               WHEN rolcanlogin AND password_set AND valid_until_finite \
                 AND valid_until::timestamptz > now() \
                 AND memberships = ARRAY[{acl}]::text[] \
                 AND connect_databases = ARRAY['wamn_system']::text[] \
                 AND (SELECT count(*) = 1 AND bool_and(object_type = 'database' \
                   AND object_name = 'wamn_system' AND privilege_type = 'CONNECT' \
                   AND NOT is_grantable) FROM grants) \
                 AND EXISTS (SELECT FROM stable WHERE \
                   NOT (rolcanlogin OR rolsuper OR rolinherit OR rolcreaterole OR rolcreatedb \
                     OR rolreplication OR rolbypassrls OR password_set) \
                   AND cardinality(memberships) = 0 AND cardinality(connect_databases) = 0 \
                   AND generation_children_exact AND owned_objects = 0) \
                 AND NOT EXISTS (SELECT FROM pg_shdepend d JOIN pg_roles r ON r.oid = d.refobjid \
                   WHERE d.refclassid = 'pg_authid'::regclass AND r.rolname IN ({role}, {acl}) \
                     AND d.dbid NOT IN (0, (SELECT oid FROM pg_database WHERE datname = 'wamn_system'))) \
                 AND NOT EXISTS (SELECT FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) a \
                   JOIN pg_roles r ON r.oid = a.grantee WHERE r.rolname IN ({role}, {acl}) \
                     AND (r.rolname = {acl} OR d.datname <> 'wamn_system' \
                       OR a.privilege_type <> 'CONNECT' OR a.is_grantable)) THEN 'active' \
               ELSE 'invalid' END \
           ELSE 'invalid' END FROM state) END"
    )
}

fn state_request(credential: &Credential, image: &str) -> JobRequest {
    let queries = credential
        .roles
        .iter()
        .map(|role| format!("({})", generation_state_sql(role, credential.acl)))
        .collect::<Vec<_>>()
        .join(", ");
    // Reusing this Job name collects a pending credential handoff BEFORE observing state.
    probe_request(
        &credential.request.name,
        image,
        "postgres",
        &format!("SELECT json_build_array({queries})"),
    )
}

fn probe_request(name: &str, image: &str, user: &str, query: &str) -> JobRequest {
    JobRequest {
        name: name.into(),
        image: image.into(),
        command: vec!["psql".into()],
        arguments: [
            "--no-psqlrc",
            "--no-password",
            "--tuples-only",
            "--no-align",
            "--set",
            "ON_ERROR_STOP=1",
            "--command",
            query,
        ]
        .map(str::to_owned)
        .into(),
        environment: [
            ("PGHOST".into(), DATABASE_HOST.into()),
            ("PGUSER".into(), user.into()),
            ("PGDATABASE".into(), "wamn_system".into()),
        ]
        .into(),
        ..JobRequest::default()
    }
}

async fn credential(
    target: &Target,
    directory: &Path,
    postgres: &str,
    admin: &JobSecret,
    password: &JobSecret,
    credential: &Credential,
) -> anyhow::Result<()> {
    let request = state_request(credential, postgres);
    let observed = job::run(target, directory, &request, std::slice::from_ref(password))
        .await
        .with_context(|| recovery(credential, "a"))?;
    let mut states: [GenerationState; 2] = serde_json::from_str(observed.stdout.trim())
        .context("decode bootstrap credential database state")?;
    let mut secret: Option<SecretData> = kubectl::get(
        &target.kubeconfig,
        &target.context,
        credential.namespace,
        "secret",
        &credential.secret,
    )
    .await?;
    if secret.is_none() {
        for (generation, state) in ["a", "b"].into_iter().zip(&states) {
            ensure!(
                matches!(state, GenerationState::Absent | GenerationState::Inactive),
                "{}",
                recovery(credential, generation)
            );
        }
        ensure!(
            states[1] == GenerationState::Absent,
            "{}",
            recovery(credential, "b")
        );
        job::run(
            target,
            directory,
            &credential.request,
            std::slice::from_ref(admin),
        )
        .await
        .with_context(|| recovery(credential, "a"))?;
        secret = kubectl::get(
            &target.kubeconfig,
            &target.context,
            credential.namespace,
            "secret",
            &credential.secret,
        )
        .await?;
        let observed =
            job::run(target, directory, &request, std::slice::from_ref(password)).await?;
        states = serde_json::from_str(observed.stdout.trim())
            .context("decode prepared credential database state")?;
    }
    let secret = secret.context("credential Job completed without its Secret")?;
    let url = secret_text(&secret, "url")?;
    let (generation, password) = credential_password(credential, &url)?;
    ensure!(
        states[generation] == GenerationState::Active,
        "{}",
        recovery(credential, if generation == 0 { "a" } else { "b" })
    );
    let role = &credential.roles[generation];
    let probe = probe_request(
        &credential.request.name,
        postgres,
        role,
        &format!(
            "SELECT current_user::text = {} AND current_database() = 'wamn_system'",
            wamn_pg_core::quote_literal(role)
        ),
    );
    let result = job::run(target, directory, &probe, &[password])
        .await
        .with_context(|| {
            format!(
                "authenticate Secret {}/{}",
                credential.namespace, credential.secret
            )
        })?;
    ensure!(
        result.stdout.trim() == "t",
        "credential authenticated with an unexpected database scope"
    );
    Ok(())
}

fn credential_password(credential: &Credential, raw: &str) -> anyhow::Result<(usize, JobSecret)> {
    let url = url::Url::parse(raw)
        .map_err(|_| anyhow::anyhow!("bootstrap credential is not a PostgreSQL URL"))?;
    ensure!(
        matches!(url.scheme(), "postgres" | "postgresql")
            && url.host_str() == Some(DATABASE_HOST)
            && url.port().is_none_or(|port| port == 5432)
            && url.path() == "/wamn_system"
            && url.query().is_none()
            && url.fragment().is_none()
            && !raw.chars().any(char::is_whitespace),
        "bootstrap credential must name the platform system database without overrides"
    );
    let generation = credential
        .roles
        .iter()
        .position(|role| role == url.username())
        .context("bootstrap credential does not name its scoped A or B login")?;
    let configuration: tokio_postgres::Config = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("bootstrap credential is not a PostgreSQL connection"))?;
    let password = configuration
        .get_password()
        .filter(|password| !password.is_empty())
        .context("bootstrap credential has no password")?;
    Ok((
        generation,
        JobSecret {
            name: "wamn-bootstrap-authenticate".into(),
            key: "password".into(),
            value: std::str::from_utf8(password)
                .map_err(|_| anyhow::anyhow!("bootstrap credential password is not UTF-8"))?
                .into(),
            environment: "PGPASSWORD".into(),
        },
    ))
}

fn recovery(credential: &Credential, generation: &str) -> String {
    format!(
        "credential {}/{} generation {generation} needs repair. Recover Job platform/{} and its Secret first. \
         If the handoff is lost, use the existing guarded `wamn-ctl {} {generation}` with the system administrator credential, \
         then prepare that named generation and install its Secret. Abort refuses a generation with live sessions; do not reset the database",
        credential.namespace, credential.secret, credential.request.name, credential.repair
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> ClusterDocument {
        serde_json::from_value(serde_json::json!({
            "provider":"kind", "project_id":"", "zone":"", "cluster_name":"test",
            "org":"acme", "owner_email":"owner@example.com", "registry":"localhost:5001",
            "buckets":{"web":"", "otel":"", "state":""},
            "route_hosts":{"control":"control.example.com"}, "resend_from":""
        }))
        .unwrap()
    }

    #[test]
    fn retained_jobs_use_ctl_and_only_the_initial_platform_scope() {
        let document = document();
        let image = "registry/ctl@sha256:abc";
        for request in provisioning_jobs(&document, image) {
            assert_eq!(request.image, image);
            assert!(!request.calls_kubernetes);
            assert!(
                super::super::job_contract::access(&request)
                    .unwrap()
                    .is_empty()
            );
            assert!(request.files.is_empty());
            assert!(request.environment.is_empty());
            let contract = super::super::job_contract::request(&request).unwrap();
            assert_eq!(contract.program, "wamn-ctl");
            let argument = |flag| {
                request
                    .arguments
                    .windows(2)
                    .find(|pair| pair[0] == flag)
                    .map(|pair| pair[1].as_str())
            };
            match contract.verb {
                "provision-system" => {
                    assert_eq!(argument("--platform-domain"), Some("example.com"));
                    assert_eq!(argument("--emit-apply-secret"), None);
                    assert!(!request.emits_secret);
                }
                "provision-org" => {
                    assert_eq!(argument("--org"), Some(document.org.as_str()));
                    assert_eq!(argument("--admin"), Some(document.owner_email.as_str()));
                    assert_eq!(argument("--template"), Some("trials"));
                    assert_eq!(argument("--pool"), Some("wamn-pg"));
                    assert_eq!(argument("--project"), None);
                    if request.emits_secret {
                        assert_eq!(argument("--prepare-control-generation"), Some("a"));
                        assert_eq!(argument("--namespace"), Some("hosts"));
                        assert_eq!(argument("--emit-control-secret"), Some("/out/secret.json"));
                        assert_eq!(argument("--db-host"), Some(DATABASE_HOST));
                    }
                }
                "provision-identity-issuer" => {
                    assert!(request.emits_secret);
                    assert_eq!(argument("--issuer"), Some(super::super::identity::ISSUER));
                    assert_eq!(argument("--prepare-generation"), Some("a"));
                    assert_eq!(argument("--namespace"), Some("identity"));
                    assert_eq!(argument("--emit-secret"), Some("/out/secret.json"));
                    assert_eq!(argument("--db-host"), Some(DATABASE_HOST));
                }
                other => panic!("unexpected bootstrap operation {other}"),
            }
        }
    }

    #[test]
    fn credential_probes_collect_the_preparation_job_and_need_no_namespace_permissions() {
        let document = document();
        let [_, _, issuer, control] = provisioning_jobs(&document, "ctl-image");
        for service in service_credentials(&document, [issuer, control]).unwrap() {
            let request = state_request(&service, "postgres-image");
            assert_eq!(request.name, service.request.name);
            assert_eq!(request.image, "postgres-image");
            assert!(!request.emits_secret);
            assert!(!request.calls_kubernetes);
            assert!(
                super::super::job_contract::access(&request)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(request.environment["PGHOST"], DATABASE_HOST);
            assert_eq!(request.environment["PGDATABASE"], "wamn_system");
            assert!(!request.environment.contains_key("PGPASSWORD"));
            assert!(
                request
                    .arguments
                    .windows(2)
                    .any(|pair| pair == ["--set", "ON_ERROR_STOP=1"])
            );
        }
    }

    #[test]
    fn service_credentials_accept_both_scoped_generations_and_refuse_url_overrides() {
        let document = document();
        let [_, _, issuer, control] = provisioning_jobs(&document, "ctl-image");
        for service in service_credentials(&document, [issuer, control]).unwrap() {
            for (index, role) in service.roles.iter().enumerate() {
                let url =
                    format!("postgresql://{role}:private%40value@{DATABASE_HOST}/wamn_system");
                let (generation, password) = credential_password(&service, &url).unwrap();
                assert_eq!(generation, index);
                assert_eq!(password.value, "private@value");
                assert!(!format!("{password:?}").contains("private"));
                for invalid in [
                    url.replace(role, "postgres"),
                    url.replace(DATABASE_HOST, "other-db"),
                    url.replace("wamn_system", "other_database"),
                    format!("{url}?options=-crole=postgres"),
                    format!("{url}#private"),
                    url.replace(":private%40value", ""),
                ] {
                    let error = credential_password(&service, &invalid).unwrap_err();
                    assert!(!error.to_string().contains("private"));
                }
            }
        }
    }

    #[test]
    fn administrator_encodes_the_password() {
        let secret = SecretData {
            data: [("password".into(), STANDARD.encode("p@ss:/?#%"))].into(),
        };
        let admin = administrator(&secret).unwrap();
        assert_eq!(
            admin.value,
            format!("postgresql://postgres:p%40ss%3A%2F%3F%23%25@{DATABASE_HOST}/wamn_system")
        );
        assert!(!format!("{admin:?}").contains("p%40ss"));
    }

    #[test]
    fn secret_decode_errors_do_not_expose_values() {
        for value in ["private!".to_owned(), STANDARD.encode([0xff])] {
            let secret = SecretData {
                data: [("password".into(), value.clone())].into(),
            };
            let error = secret_text(&secret, "password").unwrap_err();
            assert!(!format!("{error:#}").contains(&value));
        }
    }
}
