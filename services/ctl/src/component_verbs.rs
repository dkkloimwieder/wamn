//! Integration administration through control host routes.

use std::path::PathBuf;

use crate::route_request::request as route_request;
use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use serde_json::Value;
use wamn_catalog::{HostAttachment, HostRouteSet};

/// A route request with a separate, write-only credential file.
#[derive(Debug, Args)]
pub struct ConfigurationArgs {
    /// Base URL of the control host for the organization.
    #[arg(long, env = "WAMN_CONTROL_URL", hide_env_values = true)]
    pub host_url: String,
    /// The caller's session token.
    #[arg(long, env = "WAMN_TOKEN", hide_env_values = true)]
    pub token: String,
    /// JSON request with project, name, environment, and route parameters.
    #[arg(long)]
    pub file: PathBuf,
    /// Credential value. Write, test and set-credential accept this file.
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
    Test(ConfigurationArgs),
    Copy(ConfigurationArgs),
    SetCredential(ConfigurationArgs),
    Authorize(ConfigurationArgs),
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
        .chain(HostRouteSet::Application.attachments())
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
            matches!(
                operation,
                "connection.write" | "connection.test" | "connection.set_credential"
            ),
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

/// Call one route with the caller's token and print its response.
pub async fn configuration(args: ConfigurationArgs, operation: &str) -> anyhow::Result<()> {
    let value = request(&args, operation)?;
    call(&args.host_url, &args.token, operation, &value).await
}

/// Call a control route with an already assembled request value.
pub(crate) async fn call(
    host_url: &str,
    token: &str,
    operation: &str,
    value: &Value,
) -> anyhow::Result<()> {
    let value = invoke(host_url, token, operation, value).await?;
    println!("{value}");
    Ok(())
}

/// Call one route and retain its result for adapters that follow an operation.
pub(crate) async fn invoke(
    host_url: &str,
    token: &str,
    operation: &str,
    value: &Value,
) -> anyhow::Result<Value> {
    let attachment = route(operation)?;
    let request = route_request(host_url, token, attachment, value)?;
    let response = crate::route_request::send(request)
        .await
        .context("call control route")?;
    let status = response.status();
    let body = response.into_body();
    ensure!(
        status.is_success(),
        "{operation} answered HTTP {status}: {}",
        String::from_utf8_lossy(&body)
    );
    let [mut answer]: [Value; 1] =
        serde_json::from_slice(&body).context("control route answers one request element")?;
    if let Some(error) = answer.get("error") {
        anyhow::bail!("{operation} refused: {error}");
    }
    answer
        .as_object_mut()
        .context("control response object")?
        .remove("value")
        .context("control response value")
}

pub async fn connection(args: ConnectionArgs) -> anyhow::Result<()> {
    let (operation, args) = match args.command {
        ConnectionCommand::Write(args) => ("connection.write", args),
        ConnectionCommand::Test(args) => ("connection.test", args),
        ConnectionCommand::Copy(args) => ("connection.copy", args),
        ConnectionCommand::SetCredential(args) => ("connection.set_credential", args),
        ConnectionCommand::Authorize(args) => ("connection.authorize", args),
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
    use serde_json::json;

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
            &args().host_url,
            &args().token,
            route("connection.get").unwrap(),
            &value,
        )
        .unwrap();
        assert_eq!(request.method(), hyper::Method::GET);
        assert_eq!(request.uri().path(), "/wamn_control/connection/get");
        assert!(hyper::body::Body::size_hint(request.body()).exact() == Some(0));
        assert_eq!(
            Value::Object(
                url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
                    .map(|(name, value)| (name.into_owned(), serde_json::from_str(&value).unwrap()))
                    .collect()
            ),
            value
        );
    }

    #[tokio::test]
    async fn writes_send_one_request_element_and_keep_credentials_out_of_the_url() {
        let value = json!({"project":"app","env":"dev","name":"erp","credential":"private-value"});
        let request = route_request(
            &args().host_url,
            &args().token,
            route("connection.set_credential").unwrap(),
            &value,
        )
        .unwrap();
        assert_eq!(request.method(), hyper::Method::POST);
        assert!(request.uri().query().is_none());
        let body: Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(request.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert!(body[0]["request_id"].is_string());
        assert_eq!(body[0]["value"], value);
    }
}
