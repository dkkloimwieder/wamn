//! Arguments and output of the `provision-identity-issuer`,
//! `grant-project-env-membership` and `revoke-project-env-membership` verbs.

use std::fmt;

use clap::Args;
use serde_json::json;
use std::path::PathBuf;

/// Create one API service with authored application roles and a private PAT output.
#[derive(Args)]
pub struct ApiServiceArgs {
    #[arg(long)]
    pub project: wamn_registry::ProjectId,
    #[arg(long)]
    pub env: wamn_registry::EnvName,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub display_name: String,
    #[arg(long, required = true)]
    pub role: Vec<wamn_identity_protocol::RoleName>,
    #[arg(long)]
    pub lifetime_seconds: u64,
    #[arg(long)]
    pub emit_secret: PathBuf,
}
impl fmt::Debug for ApiServiceArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiServiceArgs").finish_non_exhaustive()
    }
}

pub async fn create_api_service(args: ApiServiceArgs) -> anyhow::Result<()> {
    wamn_environment::provision_project_env::ensure_secret_path(
        &args.emit_secret,
        "API service PAT",
    )?;
    let token = crate::operation_verbs::ControlHost::from_env()?.call("api_service.create", &json!({
        "project":args.project,"env":args.env,"name":args.name,"display_name":args.display_name,
        "roles":args.role,"lifetime_seconds":args.lifetime_seconds,
    })).await?;
    wamn_environment::provision_project_env::write_secret_json(&args.emit_secret, &token)?;
    println!("created API service principal_id={}", token["principal_id"]);
    Ok(())
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
    pub project: wamn_registry::ProjectId,

    /// Exact environment within the project.
    #[arg(long)]
    pub env: wamn_registry::EnvName,

    /// Existing user principal UUID from the system database.
    #[arg(long)]
    pub principal_id: wamn_identity_protocol::PrincipalId,
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

    use super::ProjectEnvMembershipArgs;

    #[derive(Parser)]
    struct MembershipCli {
        #[command(flatten)]
        args: ProjectEnvMembershipArgs,
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
            {
                let observed = &(body[0]["value"]);
                assert_eq!(observed["project"], serde_json::json!("billing"));
                assert_eq!(observed["env"], serde_json::json!("dev"));
                assert_eq!(
                    observed["principal_id"],
                    serde_json::json!("00112233-4455-6677-8899-aabbccddeeff")
                );
            };
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
    fn exact_membership_arguments_parse() {
        let args = MembershipCli::try_parse_from(membership_arguments())
            .unwrap()
            .args;
        assert_eq!(args.host_url, "https://control.example");
        assert_eq!(args.token, "session");
        assert_eq!(args.project, "billing");
        assert_eq!(args.env, "dev".parse().unwrap());
        assert_eq!(
            args.principal_id,
            "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap()
        );
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
