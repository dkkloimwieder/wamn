//! Arguments and output of the `provision-identity-issuer`,
//! `grant-project-env-membership`, `revoke-project-env-membership`, and
//! `invite` verbs.

use std::fmt;

use clap::Args;
use serde_json::json;
use std::path::PathBuf;
use wamn_environment_control::identity_issuer::{IdentityIssuerRequest, provision_identity_issuer};
use wamn_environment_control::invite::{InviteRequest, invite as run_invite};
use wamn_platform_identity::org::MemberGrants;

use crate::provisioning_verbs::PatIssuerArgs;

/// Create one API service with authored application roles and a private PAT output.
#[derive(Args)]
pub struct ApiServiceArgs {
    #[arg(long)]
    pub org: String,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub env: String,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub display_name: String,
    #[arg(long, required = true)]
    pub role: Vec<wamn_identity_protocol::RoleName>,
    #[arg(long)]
    pub lifetime_seconds: u64,
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,
    #[arg(long, hide_env_values = true)]
    pub application_database_url: String,
    #[arg(long)]
    pub emit_secret: PathBuf,
    #[command(flatten)]
    pub pat_issuer: PatIssuerArgs,
}
impl fmt::Debug for ApiServiceArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiServiceArgs").finish_non_exhaustive()
    }
}

pub async fn create_api_service(args: ApiServiceArgs) -> anyhow::Result<()> {
    let changes = if std::env::var_os("WAMN_NATS_URL").is_some() {
        wamn_broker::Publisher::connect(&wamn_broker::nats::PlatformNats::from_env()?).await?
    } else {
        wamn_broker::Publisher::default()
    };
    let principal = wamn_environment_control::api_service::create(
        &wamn_environment_control::api_service::Request {
            org: args.org,
            project: args.project,
            env: args.env,
            name: args.name,
            display_name: args.display_name,
            roles: args.role,
            lifetime: std::time::Duration::from_secs(args.lifetime_seconds),
            system_database_url: args.system_database_url,
            application_database_url: args.application_database_url,
            identity: args.pat_issuer.into(),
            emit_secret: args.emit_secret,
        },
        &changes,
    )
    .await?;
    println!("created API service principal_id={principal}");
    Ok(())
}

/// Provisioning inputs for one identity authority, not a project environment.
#[derive(Args)]
pub struct IdentityIssuerArgs {
    /// Exact HTTPS issuer configured on wamn-identity.
    #[arg(long)]
    pub issuer: String,
    /// Administrator URL for the wamn_system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,
    /// Host the emitted credential URL names, for example `<cluster>-rw`. It
    /// has no default, so the admin URL's host never reaches a workload.
    #[arg(long)]
    pub db_host: Option<String>,
    /// Port the emitted credential URL names.
    #[arg(long, default_value_t = 5432)]
    pub db_port: u16,
    /// Namespace for the emitted Secret.
    #[arg(long, default_value = "wamn-system")]
    pub namespace: String,
    /// Name for the emitted Secret, matching the identity chart.
    #[arg(long, default_value = "wamn-identity-db")]
    pub secret_name: String,
}

impl fmt::Debug for IdentityIssuerArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityIssuerArgs")
            .field("system_database_url", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// Arguments that name one existing user and one existing project environment.
#[derive(Debug, Args)]
pub struct ProjectEnvMembershipArgs {
    /// Base URL of the control host for the organization.
    #[arg(long, env = "WAMN_CONTROL_URL", hide_env_values = true)]
    pub host_url: String,

    /// The caller's session token.
    #[arg(long, env = "WAMN_TOKEN", hide_env_values = true)]
    pub token: String,

    /// Project within the organization.
    #[arg(long)]
    pub project: String,

    /// Exact environment within the project.
    #[arg(long)]
    pub env: String,

    /// Existing user principal UUID from the system database.
    #[arg(long)]
    pub principal_id: String,
}

/// Provision the first org admin through SQL as the Provisioning platform principal.
/// An org with an active org admin refuses this command. Use user.invite afterward.
#[derive(Debug, Args)]
pub struct InviteArgs {
    /// The user's email. Identity creates the user or reuses the user that
    /// has it.
    #[arg(long)]
    pub email: String,

    /// The display name of a new user.
    #[arg(long)]
    pub display_name: String,

    /// The org the user joins.
    #[arg(long)]
    pub org: String,

    /// Grant `org-admin` to the first administrator of the org.
    #[arg(long, required = true)]
    pub org_admin: bool,

    /// A project of the org where the user gets `project-admin`. Repeatable.
    #[arg(long = "project-admin", value_name = "PROJECT")]
    pub project_admins: Vec<String>,

    /// An environment of the org, as `<project>/<env>`, where the user gets a
    /// membership. Repeatable.
    #[arg(long = "membership", value_name = "PROJECT/ENV", value_parser = parse_membership)]
    pub memberships: Vec<(String, String)>,

    /// Provisioning administrator URL for the system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,

    #[command(flatten)]
    pub pat_issuer: PatIssuerArgs,
}

/// Split `<project>/<env>`.
fn parse_membership(value: &str) -> Result<(String, String), String> {
    match value.split_once('/') {
        Some((project, env)) if !project.is_empty() && !env.is_empty() && !env.contains('/') => {
            Ok((project.to_owned(), env.to_owned()))
        }
        _ => Err("a membership is <project>/<env>".to_owned()),
    }
}

/// Run one identity credential generation action and print what it did.
pub async fn provision_issuer(args: IdentityIssuerArgs) -> anyhow::Result<()> {
    let outcome = provision_identity_issuer(&IdentityIssuerRequest {
        issuer: args.issuer,
        system_database_url: args.system_database_url,
        db_host: args.db_host,
        db_port: args.db_port,
        namespace: args.namespace,
        secret_name: args.secret_name,
    })
    .await?;
    println!("identity database role {} provisioned", outcome.role);
    Ok(())
}

/// Invite one user to an org and print what it did. Identity's reply carries
/// no token, because identity mails the invitation secret.
///
/// The platform's bootstrap write: Provisioning creates the first admin;
/// later invitations use user.invite. One system database transaction runs as
/// `wamn_system` with the Provisioning platform principal bound.
pub async fn invite(args: InviteArgs) -> anyhow::Result<()> {
    let outcome = run_invite(
        &InviteRequest {
            email: args.email,
            display_name: args.display_name,
            org: args.org.clone(),
            grants: MemberGrants {
                org_admin: args.org_admin,
                project_admins: args.project_admins,
                memberships: args.memberships,
            },
            system_database_url: args.system_database_url,
            identity: args.pat_issuer.into(),
        },
        &wamn_broker::Publisher::connect(&wamn_broker::nats::PlatformNats::from_env()?).await?,
    )
    .await?;
    match outcome.invitation {
        Some(reply) => println!(
            "invited principal_id={} org={}: {} {}",
            outcome.principal_id,
            args.org,
            reply.status,
            reply.body.trim()
        ),
        None => println!(
            "invited principal_id={} org={}: enrolled, no invitation sent",
            outcome.principal_id, args.org
        ),
    }
    Ok(())
}

/// Grant one user's project environment membership and print the new state.
pub async fn grant(args: ProjectEnvMembershipArgs) -> anyhow::Result<()> {
    membership(args, "member.grant").await
}

/// Revoke one user's project environment membership and print the new state.
pub async fn revoke(args: ProjectEnvMembershipArgs) -> anyhow::Result<()> {
    membership(args, "member.revoke").await
}

async fn membership(args: ProjectEnvMembershipArgs, operation: &str) -> anyhow::Result<()> {
    crate::component_verbs::call(
        &args.host_url,
        &args.token,
        operation,
        &json!({"project": args.project, "env": args.env, "principal_id": args.principal_id}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser};
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

    use super::{IdentityIssuerArgs, InviteArgs, ProjectEnvMembershipArgs};

    #[derive(Parser)]
    struct IssuerCli {
        #[command(flatten)]
        args: IdentityIssuerArgs,
    }

    #[derive(Parser)]
    struct MembershipCli {
        #[command(flatten)]
        args: ProjectEnvMembershipArgs,
    }

    #[derive(Parser)]
    struct InviteCli {
        #[command(flatten)]
        args: InviteArgs,
    }

    async fn membership_server(path: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(line, format!("POST {path} HTTP/1.1\r\n"));
            let mut length = None;
            let mut authorization = None;
            loop {
                line.clear();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
                if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_owned());
                }
            }
            assert_eq!(authorization.as_deref(), Some("Bearer session"));
            let mut body = vec![0; length.unwrap()];
            stream.read_exact(&mut body).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body.as_array().unwrap().len(), 1);
            assert!(!body[0]["request_id"].as_str().unwrap().is_empty());
            assert_eq!(
                body[0]["value"],
                serde_json::json!({
                    "project": "billing", "env": "dev",
                    "principal_id": "00112233-4455-6677-8899-aabbccddeeff"
                })
            );
            let body = r#"[{"value":{}}]"#;
            stream
                .write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ).as_bytes())
                .await
                .unwrap();
        });
        (host_url, server)
    }

    #[tokio::test]
    async fn grant_posts_to_member_grant() {
        let (host_url, server) = membership_server("/wamn_control/member/grant").await;
        let mut args = MembershipCli::try_parse_from(membership_arguments())
            .unwrap()
            .args;
        args.host_url = host_url;
        super::grant(args).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn revoke_posts_to_member_revoke() {
        let (host_url, server) = membership_server("/wamn_control/member/revoke").await;
        let mut args = MembershipCli::try_parse_from(membership_arguments())
            .unwrap()
            .args;
        args.host_url = host_url;
        super::revoke(args).await.unwrap();
        server.await.unwrap();
    }

    fn invite_arguments() -> Vec<&'static str> {
        vec![
            "invite",
            "--email",
            "ann@example.test",
            "--display-name",
            "Ann",
            "--org",
            "acme",
            "--org-admin",
            "--system-database-url",
            "postgres://admin:secret@localhost/wamn_system",
        ]
    }

    #[test]
    fn invite_arguments_repeat_grants_and_split_memberships() {
        let mut values = invite_arguments();
        values.extend([
            "--project-admin",
            "billing",
            "--project-admin",
            "shop",
            "--membership",
            "billing/dev",
            "--membership",
            "shop/prod",
        ]);
        let args = InviteCli::try_parse_from(values).unwrap().args;
        assert!(args.org_admin);
        assert_eq!(args.project_admins, ["billing", "shop"]);
        assert_eq!(
            args.memberships,
            [
                ("billing".to_owned(), "dev".to_owned()),
                ("shop".to_owned(), "prod".to_owned())
            ]
        );
        let args = InviteCli::try_parse_from(invite_arguments()).unwrap().args;
        assert!(args.org_admin && args.project_admins.is_empty() && args.memberships.is_empty());
        for extra in [
            ["--membership", "billing"],
            ["--membership", "billing/dev/x"],
            ["--membership", "/dev"],
            ["--principal", "00112233-4455-6677-8899-aabbccddeeff"],
        ] {
            let mut values = invite_arguments();
            values.extend(extra);
            assert!(InviteCli::try_parse_from(values).is_err(), "{extra:?}");
        }
        let mut values = invite_arguments();
        values.retain(|value| *value != "--org-admin");
        assert!(InviteCli::try_parse_from(values).is_err());
    }

    fn issuer_arguments() -> Vec<&'static str> {
        vec![
            "issuer",
            "--issuer",
            "https://identity.wamn-system.svc",
            "--system-database-url",
            "postgres://admin:hidden-value@sysdb/wamn_system",
            "--db-host",
            "sysdb",
        ]
    }

    fn membership_arguments() -> [&'static str; 11] {
        [
            "membership",
            "--host-url",
            "https://control.example",
            "--project",
            "billing",
            "--env",
            "dev",
            "--principal-id",
            "00112233-4455-6677-8899-aabbccddeeff",
            "--token",
            "session",
        ]
    }

    #[test]
    fn targeted_role_arguments_parse_and_redact_admin_credentials() {
        let args = IssuerCli::try_parse_from(issuer_arguments()).unwrap().args;
        assert_eq!(args.namespace, "wamn-system");
        assert_eq!(args.secret_name, "wamn-identity-db");
        assert_eq!(args.db_host.as_deref(), Some("sysdb"));
        assert!(!format!("{args:?}").contains("hidden-value"));
    }

    #[test]
    fn an_issuer_rejects_an_unrelated_org_argument() {
        let mut values = issuer_arguments();
        values.extend(["--org", "acme"]);
        assert!(IssuerCli::try_parse_from(values).is_err());
    }

    #[test]
    fn exact_membership_arguments_parse() {
        let args = MembershipCli::try_parse_from(membership_arguments())
            .unwrap()
            .args;
        assert_eq!(args.host_url, "https://control.example");
        assert_eq!(args.token, "session");
        assert_eq!(args.project, "billing");
        assert_eq!(args.env, "dev");
        assert_eq!(args.principal_id, "00112233-4455-6677-8899-aabbccddeeff");
    }

    #[test]
    fn every_membership_argument_is_required() {
        let command = MembershipCli::command();
        for name in ["host_url", "token", "project", "env", "principal_id"] {
            let argument = command
                .get_arguments()
                .find(|argument| argument.get_id() == name)
                .unwrap();
            assert!(argument.is_required_set(), "{name} must be required");
        }
        for (name, environment) in [("host_url", "WAMN_CONTROL_URL"), ("token", "WAMN_TOKEN")] {
            let argument = command
                .get_arguments()
                .find(|argument| argument.get_id() == name)
                .unwrap();
            assert_eq!(argument.get_env().unwrap(), environment);
        }
    }
}
