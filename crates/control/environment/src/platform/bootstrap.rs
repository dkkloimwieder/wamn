//! System initialization Jobs and redacted Kubernetes credential decoding.

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
use super::job::{self, Secret};
use crate::kubectl;

pub(super) fn database_host() -> String {
    crate::provision_project_env::database_host(None, "wamn-pg", "platform").into_owned()
}

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

pub(crate) fn secret_text(secret: &SecretData, key: &str) -> anyhow::Result<String> {
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
pub(super) fn administrator(job: &mut Value, secret: &SecretData) -> anyhow::Result<Secret> {
    let mut url = url::Url::parse(&format!(
        "postgresql://postgres@{}/wamn_system",
        database_host()
    ))?;
    url.set_password(Some(&secret_text(secret, "password")?.replace('%', "%25")))
        .map_err(|()| anyhow::anyhow!("encode the system administrator password"))?;
    Ok(job::credential(
        job,
        "wamn-system-admin",
        "WAMN_SYSTEM_ADMIN_URL",
        "url",
        url.as_str(),
    ))
}

fn system_job(document: &ClusterDocument, image: &str) -> Value {
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);
    let mut job = job::manifest(
        "platform-system",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
            "provision-system",
            "--platform-domain",
            domain,
            "--emit-apply-secret",
            "/out/secret.json",
            "--db-host",
            &database_host(),
        ],
    );
    job::secret_output(&mut job);
    job
}

fn organization_job(document: &ClusterDocument, image: &str) -> Value {
    job::manifest(
        "platform-org",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
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
    )
}

fn issuer_credential_job(image: &str) -> Value {
    let mut job = job::manifest(
        "platform-identity-issuer",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
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
            &database_host(),
        ],
    );
    job::secret_output(&mut job);
    job
}

fn control_credential_job(document: &ClusterDocument, image: &str) -> Value {
    let mut job = organization_job(document, image);
    job["metadata"]["name"] = format!("platform-control-credentials-{}", document.org).into();
    job::container(&mut job)["name"] = job["metadata"]["name"].clone();
    job::container(&mut job)["args"]
        .as_array_mut()
        .unwrap()
        .extend(
            [
                "--prepare-control-generation",
                "a",
                "--namespace",
                "hosts",
                "--emit-control-secret",
                "/out/secret.json",
                "--db-host",
                &database_host(),
            ]
            .map(Value::from),
        );
    job::secret_output(&mut job);
    job
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
    let image = chart.image("ctl")?;
    let mut system = system_job(document, image);
    let admin = administrator(&mut system, &superuser)?;
    job::run(&target, directory, &mut system, &mut [admin]).await?;
    let mut org = organization_job(document, image);
    let admin = administrator(&mut org, &superuser)?;
    job::run(&target, directory, &mut org, &mut [admin]).await?;

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
    for service in service_credentials(document, image)? {
        credential(&target, directory, postgres, &superuser, service).await?;
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
    job: Value,
    namespace: &'static str,
    secret: String,
    roles: [String; 2],
    acl: &'static str,
    repair: String,
}

fn service_credentials(document: &ClusterDocument, image: &str) -> anyhow::Result<[Credential; 2]> {
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
    Ok([
        Credential {
            job: issuer_credential_job(image),
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
            job: control_credential_job(document, image),
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

fn state_job(credential: &Credential, image: &str) -> Value {
    let queries = credential
        .roles
        .iter()
        .map(|role| format!("({})", generation_state_sql(role, credential.acl)))
        .collect::<Vec<_>>()
        .join(", ");
    // Reusing this Job name collects a pending credential handoff BEFORE observing state.
    probe_job(
        job::name(&credential.job),
        image,
        "postgres",
        &format!("SELECT json_build_array({queries})"),
    )
}

fn probe_job(name: &str, image: &str, user: &str, query: &str) -> Value {
    let mut job = job::manifest(
        name,
        image,
        &["psql"],
        &[
            "--no-psqlrc",
            "--no-password",
            "--tuples-only",
            "--no-align",
            "--set",
            "ON_ERROR_STOP=1",
            "--command",
            query,
        ],
    );
    job::environment(
        &mut job,
        &[
            ("PGHOST", &database_host()),
            ("PGUSER", user),
            ("PGDATABASE", "wamn_system"),
        ],
    );
    job
}

async fn credential(
    target: &Target,
    directory: &Path,
    postgres: &str,
    superuser: &SecretData,
    mut credential: Credential,
) -> anyhow::Result<()> {
    let mut probe = state_job(&credential, postgres);
    let mut password = job::credential(
        &mut probe,
        "wamn-bootstrap-probe",
        "PGPASSWORD",
        "password",
        &secret_text(superuser, "password")?,
    );
    let observed = job::run(
        target,
        directory,
        &mut probe,
        std::slice::from_mut(&mut password),
    )
    .await
    .with_context(|| recovery(&credential, "a"))?;
    let mut states: [GenerationState; 2] = serde_json::from_str(observed.trim())
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
                recovery(&credential, generation)
            );
        }
        ensure!(
            states[1] == GenerationState::Absent,
            "{}",
            recovery(&credential, "b")
        );
        let admin = administrator(&mut credential.job, superuser)?;
        job::run(target, directory, &mut credential.job, &mut [admin])
            .await
            .with_context(|| recovery(&credential, "a"))?;
        secret = kubectl::get(
            &target.kubeconfig,
            &target.context,
            credential.namespace,
            "secret",
            &credential.secret,
        )
        .await?;
        let observed = job::run(
            target,
            directory,
            &mut probe,
            std::slice::from_mut(&mut password),
        )
        .await?;
        states = serde_json::from_str(observed.trim())
            .context("decode prepared credential database state")?;
    }
    let secret = secret.context("credential Job completed without its Secret")?;
    let url = secret_text(&secret, "url")?;
    let (generation, password) = credential_password(&credential, &url)?;
    ensure!(
        states[generation] == GenerationState::Active,
        "{}",
        recovery(&credential, if generation == 0 { "a" } else { "b" })
    );
    let role = &credential.roles[generation];
    let mut probe = probe_job(
        job::name(&credential.job),
        postgres,
        role,
        &format!(
            "SELECT current_user::text = {} AND current_database() = 'wamn_system'",
            wamn_pg_core::quote_literal(role)
        ),
    );
    let password = job::credential(
        &mut probe,
        "wamn-bootstrap-authenticate",
        "PGPASSWORD",
        "password",
        &password,
    );
    let result = job::run(target, directory, &mut probe, &mut [password])
        .await
        .with_context(|| {
            format!(
                "authenticate Secret {}/{}",
                credential.namespace, credential.secret
            )
        })?;
    ensure!(
        result.trim() == "t",
        "credential authenticated with an unexpected database scope"
    );
    Ok(())
}

fn credential_password(credential: &Credential, raw: &str) -> anyhow::Result<(usize, String)> {
    let url = url::Url::parse(raw)
        .map_err(|_| anyhow::anyhow!("bootstrap credential is not a PostgreSQL URL"))?;
    ensure!(
        matches!(url.scheme(), "postgres" | "postgresql")
            && url.host_str() == Some(database_host().as_str())
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
        std::str::from_utf8(password)
            .map_err(|_| anyhow::anyhow!("bootstrap credential password is not UTF-8"))?
            .into(),
    ))
}

fn recovery(credential: &Credential, generation: &str) -> String {
    format!(
        "credential {}/{} generation {generation} needs repair. Recover Job platform/{} and its Secret first. \
         If the handoff is lost, use the existing guarded `wamn-ctl {} {generation}` with the system administrator credential, \
         then prepare that named generation and install its Secret. Abort refuses a generation with live sessions; do not reset the database",
        credential.namespace,
        credential.secret,
        job::name(&credential.job),
        credential.repair
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
    fn system_initialization_hands_off_the_apply_credential() {
        let mut system = system_job(&document(), "ctl-image");
        let container = job::container(&mut system);
        let args = container["args"].as_array().unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--emit-apply-secret", "/out/secret.json"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--db-host", "wamn-pg-rw.platform.svc.cluster.local"])
        );
        assert!(
            container["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mount| mount["mountPath"] == "/out")
        );
        assert!(
            system["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|volume| volume["name"] == "out" && volume["emptyDir"]["medium"] == "Memory")
        );
    }

    #[test]
    fn credential_probes_collect_the_preparation_job_before_reading_database_state() {
        for service in service_credentials(&document(), "ctl-image").unwrap() {
            let mut probe = state_job(&service, "postgres-image");
            assert_eq!(job::name(&probe), job::name(&service.job));
            assert_eq!(
                job::container(&mut probe)["command"],
                serde_json::json!(["psql"])
            );
        }
    }

    #[test]
    fn service_credentials_accept_both_scoped_generations_and_refuse_url_overrides() {
        let document = document();
        for service in service_credentials(&document, "ctl-image").unwrap() {
            for (index, role) in service.roles.iter().enumerate() {
                let url = format!(
                    "postgresql://{role}:private%40value@wamn-pg-rw.platform.svc.cluster.local/wamn_system"
                );
                let (generation, password) = credential_password(&service, &url).unwrap();
                assert_eq!(generation, index);
                assert_eq!(password, "private@value");
                for invalid in [
                    url.replace(role, "postgres"),
                    url.replace("wamn-pg-rw.platform.svc.cluster.local", "other-db"),
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
        let mut job = system_job(&document(), "ctl-image");
        let admin = administrator(&mut job, &secret).unwrap();
        assert_eq!(
            admin.0["stringData"]["url"],
            "postgresql://postgres:p%40ss%3A%2F%3F%23%25@wamn-pg-rw.platform.svc.cluster.local/wamn_system"
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
