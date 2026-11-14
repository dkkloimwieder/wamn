//! Deploy a fixture with product commands and observe its database without writes.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use sqlx::Row as _;
use wamn_environment::environment::document::{DeclaredRelease, EnvironmentDocument};
use wamn_environment::platform::{document::ClusterDocument, document::Provider, provider};
use wamn_test_infrastructure::scratch::ScratchRoot;

mod routes;
pub use routes::Routes;

pub struct Application {
    pub routes: Routes,
    pub caller: String,
    document: EnvironmentDocument,
    scratch: ScratchRoot,
}

impl Application {
    pub async fn prepare(source: &Path) -> anyhow::Result<Self> {
        let scratch = ScratchRoot::create()?;
        let cluster = ClusterDocument::compile(Path::new(&std::env::var("WAMN_TEST_CLUSTER")?))?;
        ensure!(
            cluster.provider == Provider::Kind,
            "fixture requires the kind tool"
        );
        let kubeconfig = scratch.path().join("kubeconfig");
        let context = provider::existing_kubeconfig(&cluster, &kubeconfig).await?;
        let namespace = wamn_kubernetes::kubectl::kubectl(
            &kubeconfig,
            &context,
            "platform",
            &["get", "namespace", "platform", "-o", "json"],
            None,
        )
        .await?;
        let namespace: Value = serde_json::from_str(&namespace)?;
        ensure!(
            namespace["metadata"]["annotations"]["wamn.dev/disposable"] == "true",
            "fixture requires a disposable platform"
        );
        let mut document = EnvironmentDocument::compile(
            &PathBuf::from(std::env::var("WAMN_DEV_ENV")?).join("environment.k"),
        )?;
        ensure!(
            document.org == cluster.org,
            "fixture org differs from the platform"
        );
        ensure!(
            document.applications.len() == 1,
            "select one fixture application"
        );
        let source_environment = document.env.clone();
        document.env = format!("route-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let (name, application) = document
            .applications
            .first_key_value()
            .context("application")?;
        let name = name.to_owned();
        let hostname = application
            .hostname
            .split_once('.')
            .context("fixture hostname domain")?
            .1;
        let hostname = format!("{}.{hostname}", document.env);
        document
            .applications
            .get_mut(&name)
            .context("application")?
            .hostname = hostname.clone();
        eprintln!("Product fixture environment: {}", document.triple());
        document
            .applications
            .get_mut(&name)
            .context("application")?
            .current = DeclaredRelease::None;
        let file = scratch.path().join("environment.k");
        std::fs::write(&file, document.to_kcl())?;
        product(["env", "apply", text(&file)?, "--disposable"]).await?;
        for connection in document.applications[&name].connections.values() {
            let request = scratch.path().join("connection.json");
            std::fs::write(
                &request,
                serde_json::to_vec(&json!({
                    "project":document.project,"env":document.env,"name":connection,"source_env":source_environment
                }))?,
            )?;
            product(["connection", "copy", "--file", text(&request)?]).await?;
            std::fs::write(
                &request,
                serde_json::to_vec(&json!({
                    "project":document.project,"env":document.env,"name":connection
                }))?,
            )?;
            product(["connection", "enable", "--file", text(&request)?]).await?;
        }
        deploy(&file, source).await?;
        let secret = scratch.path().join("caller.json");
        product([
            "create-api-service",
            "--project",
            &document.project,
            "--env",
            &document.env,
            "--name",
            &document.env,
            "--display-name",
            "Route fixture",
            "--role",
            "admin",
            "--lifetime-seconds",
            "900",
            "--emit-secret",
            text(&secret)?,
        ])
        .await?;
        let caller: Value = serde_json::from_slice(&std::fs::read(&secret)?)?;
        let bearer = caller["token"].as_str().context("caller PAT")?.to_owned();
        let caller = caller["principal_id"]
            .as_str()
            .context("caller principal")?
            .to_owned();
        std::fs::remove_file(secret)?;
        let routes = Routes {
            endpoint: format!(
                "{}/api/{name}",
                std::env::var("WAMN_BROWSER_URL")?.trim_end_matches('/')
            ),
            host: hostname,
            bearer,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
        };
        Ok(Self {
            routes,
            caller,
            document,
            scratch,
        })
    }

    pub async fn database_url(&self) -> anyhow::Result<String> {
        let url = std::env::var("WAMN_SYSTEM_ADMIN_URL")?;
        let suffix = self.instance_suffix().await?;
        let name = wamn_control_provision::project_env_database_name(
            &self.document.org,
            &self.document.project,
            &self.document.env,
            &suffix,
        );
        wamn_postgres::database_configuration(&url, &name)
    }

    pub async fn observer(&self) -> anyhow::Result<sqlx::PgConnection> {
        let url = self.database_url().await?;
        let mut client = wamn_postgres::connect(&url).await?;
        sqlx::raw_sql("SET ROLE wamn_db_owner; SET default_transaction_read_only = on")
            .execute(&mut client)
            .await
            .map(drop)?;
        Ok(client)
    }

    pub fn target(&self) -> &EnvironmentDocument {
        &self.document
    }

    pub async fn rebuild(&self, source: &Path) -> anyhow::Result<()> {
        deploy(&self.scratch.path().join("environment.k"), source).await
    }

    pub async fn audience(&self) -> anyhow::Result<String> {
        Ok(wamn_control_provision::session_target::session_audience(
            &self.document.triple(),
            &self.instance_suffix().await?,
        )?)
    }

    async fn instance_suffix(&self) -> anyhow::Result<String> {
        let url = std::env::var("WAMN_SYSTEM_ADMIN_URL")?;
        let mut system = wamn_postgres::connect(&url).await?;
        sqlx::raw_sql("SET default_transaction_read_only = on")
            .execute(&mut system)
            .await
            .map(drop)?;
        let suffix: String = sqlx::query("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
            .bind(&self.document.org).bind(&self.document.project).bind(&self.document.env)
            .fetch_one(&mut system).await?.get(0);
        Ok(suffix)
    }

    pub async fn call(&self, operation: &str, value: Value) -> anyhow::Result<Value> {
        let (_, route) = wamn_catalog::HostRouteSet::Application
            .attachments()
            .find(|(_, route)| route.route.operation == operation)
            .context("application operation")?;
        let path = route.definition["route"]["path"]
            .as_str()
            .context("application route path")?;
        let mut answer = self
            .routes
            .post(path, json!({"request_id":operation,"value":value}))
            .await?;
        answer
            .as_object_mut()
            .context("application route answer")?
            .remove("value")
            .with_context(|| format!("{operation} refused: {answer}"))
    }

    pub async fn remove(self) -> anyhow::Result<()> {
        product(["env", "delete", &self.document.triple().to_string()]).await?;
        Ok(())
    }
}

fn text(path: &Path) -> anyhow::Result<&str> {
    path.to_str().context("fixture path must be UTF-8")
}

async fn product<const N: usize>(args: [&str; N]) -> anyhow::Result<()> {
    let output = tokio::process::Command::new(std::env::var("WAMN_CTL_BIN")?)
        .args(args)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "product command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

async fn deploy(file: &Path, source: &Path) -> anyhow::Result<()> {
    product([
        "dev",
        "--config",
        text(file)?,
        "--overlay-root",
        text(source)?,
    ])
    .await
}

impl std::fmt::Debug for Application {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Application")
            .field("document", &self.document.triple())
            .finish_non_exhaustive()
    }
}
