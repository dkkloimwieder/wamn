//! Integration administration through control host routes.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use wamn_catalog::{HostAttachment, HostRouteSet};

/// A route request with a separate, write-only credential file.
#[derive(Debug, Args)]
pub struct ConfigurationArgs {
    /// Base URL of the control host for the organization.
    #[arg(long, env = "WAMN_CONTROL_URL")]
    pub host_url: String,
    /// The caller's session token.
    #[arg(long, env = "WAMN_TOKEN", hide_env_values = true)]
    pub token: String,
    /// JSON request with project, name, environment, and route parameters.
    #[arg(long)]
    pub file: PathBuf,
    /// Credential value. Only write and set-credential accept this file.
    #[arg(long)]
    pub credential_file: Option<PathBuf>,
}

/// Environment connection administration.
#[derive(Debug, Args)]
pub struct ConnectionArgs {
    #[command(subcommand)]
    pub command: ConnectionCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConnectionCommand {
    Write(ConfigurationArgs),
    Copy(ConfigurationArgs),
    SetCredential(ConfigurationArgs),
    Disable(ConfigurationArgs),
    Enable(ConfigurationArgs),
    Delete(ConfigurationArgs),
    List(ConfigurationArgs),
    Get(ConfigurationArgs),
}

/// Project external-system administration.
#[derive(Debug, Args)]
pub struct ExternalSystemArgs {
    #[command(subcommand)]
    pub command: ExternalSystemCommand,
}

#[derive(Debug, Subcommand)]
pub enum ExternalSystemCommand {
    Write(ConfigurationArgs),
    List(ConfigurationArgs),
    Delete(ConfigurationArgs),
}

/// The platform integration catalog, read through project authority.
#[derive(Debug, Args)]
pub struct IntegrationTypeArgs {
    #[command(subcommand)]
    pub command: IntegrationTypeCommand,
}

#[derive(Debug, Subcommand)]
pub enum IntegrationTypeCommand {
    List(ConfigurationArgs),
}

pub fn route(operation: &str) -> anyhow::Result<&'static HostAttachment> {
    HostRouteSet::Control
        .attachments()
        .map(|(_, attachment)| attachment)
        .find(|attachment| attachment.route.operation == operation)
        .with_context(|| format!("control route {operation} is absent"))
}

fn request(args: &ConfigurationArgs, operation: &str) -> anyhow::Result<Value> {
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(&args.file).context("read request file")?)
            .context("parse request file")?;
    ensure!(value.is_object(), "the request must be a JSON object");
    ensure!(
        value.get("credential").is_none(),
        "use --credential-file for the write-only value"
    );
    if let Some(path) = &args.credential_file {
        ensure!(
            matches!(operation, "connection.write" | "connection.set_credential"),
            "this route does not accept a credential"
        );
        value["credential"] = std::fs::read_to_string(path)
            .context("read credential file")?
            .into();
    }
    ensure!(
        operation != "connection.set_credential" || args.credential_file.is_some(),
        "set-credential requires --credential-file"
    );
    Ok(value)
}

fn route_request(
    client: &reqwest::Client,
    args: &ConfigurationArgs,
    attachment: &HostAttachment,
    value: &Value,
) -> anyhow::Result<reqwest::Request> {
    let mut url = format!(
        "{}{}",
        args.host_url.trim_end_matches('/'),
        attachment.definition["route"]["path"]
            .as_str()
            .context("route path")?
    );
    let request = if attachment.route.type_.is_read() {
        let query = wamn_execution_contract::encode_read_query(
            value.as_object().context("read request object")?,
        );
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        client.get(url)
    } else {
        let request_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            .to_string();
        client
            .post(url)
            .json(&json!([{"request_id":request_id,"value":value}]))
    };
    request
        .bearer_auth(&args.token)
        .build()
        .context("build control request")
}

/// Call one route with the caller's token and print its response.
pub async fn configuration(args: ConfigurationArgs, operation: &str) -> anyhow::Result<()> {
    let attachment = route(operation)?;
    let value = request(&args, operation)?;
    let client = reqwest::Client::new();
    let request = route_request(&client, &args, attachment, &value)?;
    let response = client
        .execute(request)
        .await
        .context("call control route")?;
    let status = response.status();
    let body = response.text().await.context("read control response")?;
    ensure!(
        status.is_success(),
        "{operation} answered HTTP {status}: {body}"
    );
    let [answer]: [Value; 1] =
        serde_json::from_str(&body).context("control route answers one request element")?;
    if let Some(error) = answer.get("error") {
        anyhow::bail!("{operation} refused: {error}");
    }
    println!("{}", answer["value"]);
    Ok(())
}

pub async fn connection(args: ConnectionArgs) -> anyhow::Result<()> {
    let (operation, args) = match args.command {
        ConnectionCommand::Write(args) => ("connection.write", args),
        ConnectionCommand::Copy(args) => ("connection.copy", args),
        ConnectionCommand::SetCredential(args) => ("connection.set_credential", args),
        ConnectionCommand::Disable(args) => ("connection.disable", args),
        ConnectionCommand::Enable(args) => ("connection.enable", args),
        ConnectionCommand::Delete(args) => ("connection.delete", args),
        ConnectionCommand::List(args) => ("connection.list", args),
        ConnectionCommand::Get(args) => ("connection.get", args),
    };
    configuration(args, operation).await
}

pub async fn external_system(args: ExternalSystemArgs) -> anyhow::Result<()> {
    let (operation, args) = match args.command {
        ExternalSystemCommand::Write(args) => ("external_system.write", args),
        ExternalSystemCommand::List(args) => ("external_system.list", args),
        ExternalSystemCommand::Delete(args) => ("external_system.delete", args),
    };
    configuration(args, operation).await
}

pub async fn integration_type(args: IntegrationTypeArgs) -> anyhow::Result<()> {
    match args.command {
        IntegrationTypeCommand::List(args) => configuration(args, "integration_type.list").await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_catalog::{HostRouteAuthority, OperationType};

    fn args() -> ConfigurationArgs {
        ConfigurationArgs {
            host_url: "https://control.example".into(),
            token: "session".into(),
            file: PathBuf::from("/tmp/request.json"),
            credential_file: None,
        }
    }

    #[test]
    fn reads_send_the_canonical_query_and_no_body() {
        let value = json!({"project":"app","env":"dev","name":"erp"});
        let request = route_request(
            &reqwest::Client::new(),
            &args(),
            route("connection.get").unwrap(),
            &value,
        )
        .unwrap();
        assert_eq!(request.method(), reqwest::Method::GET);
        assert_eq!(request.url().path(), "/wamn_control/connection/get");
        assert!(request.body().is_none());
        assert_eq!(
            Value::Object(
                wamn_execution_contract::decode_read_query(request.url().query().unwrap()).unwrap()
            ),
            value
        );
    }

    #[test]
    fn writes_send_one_request_element_and_keep_credentials_out_of_the_url() {
        let value = json!({"project":"app","env":"dev","name":"erp","credential":"private-value"});
        let request = route_request(
            &reqwest::Client::new(),
            &args(),
            route("connection.set_credential").unwrap(),
            &value,
        )
        .unwrap();
        assert_eq!(request.method(), reqwest::Method::POST);
        assert!(request.url().query().is_none());
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert!(body[0]["request_id"].is_string());
        assert_eq!(body[0]["value"], value);
    }

    #[test]
    fn every_administration_verb_maps_to_a_project_route() {
        for (group, verbs) in [
            ("integration_type", &["list"][..]),
            ("external_system", &["write", "list", "delete"][..]),
            (
                "integration",
                &["write", "list", "get", "disable", "enable", "delete"][..],
            ),
            (
                "connection",
                &[
                    "write",
                    "copy",
                    "set_credential",
                    "disable",
                    "enable",
                    "delete",
                    "list",
                    "get",
                ][..],
            ),
        ] {
            for verb in verbs {
                let attachment = route(&format!("{group}.{verb}")).unwrap();
                assert_eq!(attachment.route.authority, HostRouteAuthority::ProjectAdmin);
                assert_eq!(
                    attachment.route.type_,
                    if matches!(*verb, "list" | "get") {
                        OperationType::Get
                    } else {
                        OperationType::Command
                    }
                );
            }
        }
    }
}
