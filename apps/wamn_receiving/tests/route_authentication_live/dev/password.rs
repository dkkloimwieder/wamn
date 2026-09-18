//! Password access to the actual identity process during a Receiving rebuild.

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_control::dev::environment::{
    DevEnvironment, ENVIRONMENT, ORG, PROJECT, TENANT, connect, reconcile_journey_run_plane,
};
use wamn_control_provision::PlatformComponent;
use wamn_platform_identity::{create_human, grant_project_env_membership};

const EMAIL: &str = "managed-development@example.invalid";
const PASSWORD: &str = "managed-development-disposable-fixture-password";

pub(super) struct Login {
    http: reqwest::Client,
    endpoint: String,
    audience: String,
    keys: Value,
}

impl Login {
    pub(super) async fn start(environment: &DevEnvironment, system: &str) -> anyhow::Result<Self> {
        let endpoint = environment
            .issuer
            .args
            .endpoint
            .clone()
            .context("owned issuer endpoint")?;
        let ca_path = environment
            .issuer
            .args
            .server_ca
            .as_ref()
            .context("owned issuer CA")?;
        let ca = std::fs::read(ca_path)?;
        let http = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca)?)
            .build()?;
        let directory = ca_path.parent().context("owned issuer directory")?;
        let target: Value = serde_json::from_slice(&std::fs::read(
            directory
                .parent()
                .context("environment directory")?
                .join("identity-target.json"),
        )?)?;
        let audience = target["audience"]
            .as_str()
            .context("configured audience")?
            .to_owned();
        let (admin, task) = connect(system).await?;
        let actor = PlatformComponent::Provisioning.principal_id().to_string();
        admin
            .execute("SELECT set_config('app.user_id', $1, false)", &[&actor])
            .await?;
        let human = create_human(&*admin, EMAIL, EMAIL, "Development fixture").await?;
        grant_project_env_membership(&*admin, human.id(), ORG, PROJECT, ENVIRONMENT).await?;
        reconcile_journey_run_plane(system, &environment.route.database_url).await?;
        let (project, project_task) = connect(&environment.route.database_url).await?;
        project.execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation','admin:seed-identity-fixture',false)", &[&actor]).await?;
        project
            .execute(
                "INSERT INTO app_system.roles (tenant_id,name) VALUES ($1,'development-fixture')",
                &[&TENANT],
            )
            .await?;
        project.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,'development-fixture')", &[&TENANT,&human.id().as_str()]).await?;
        project_task.abort();
        task.abort();
        let issuer_url = wamn_control::provision_project_env::secret_value(
            &directory.join("database.json"),
            "url",
        )?;
        let (mut issuer, connection) = tokio_postgres::connect(&issuer_url, tokio_postgres::NoTls)
            .await
            .map_err(|_| anyhow::anyhow!("connect the fixture invitation authority"))?;
        let driver = tokio::spawn(connection);
        let invitation = wamn_platform_identity::password::issue_invitation(
            &mut issuer,
            &actor.parse()?,
            human.id(),
        )
        .await?;
        driver.abort();
        let response = http.post(format!("{endpoint}/password/enroll"))
            .json(&json!({"principal_id":human.id().as_str(),"invitation":invitation.secret(),"password":PASSWORD}))
            .send().await?;
        ensure!(
            response.status() == reqwest::StatusCode::NO_CONTENT,
            "development password enrollment refused"
        );
        let keys = http
            .get(format!("{endpoint}/.well-known/jwks.json"))
            .send()
            .await?
            .json()
            .await?;
        let login = Self {
            http,
            endpoint,
            audience,
            keys,
        };
        login.available().await?;
        Ok(login)
    }

    pub(super) async fn available(&self) -> anyhow::Result<()> {
        let response = self
            .http
            .post(format!("{}/password/session", self.endpoint))
            .json(&json!({"email":EMAIL,"password":PASSWORD,"aud":self.audience}))
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "password login refused during the development lifecycle"
        );
        let keys: Value = self
            .http
            .get(format!("{}/.well-known/jwks.json", self.endpoint))
            .send()
            .await?
            .json()
            .await?;
        ensure!(
            keys == self.keys,
            "the application rebuild changed identity signing keys"
        );
        Ok(())
    }

    pub(super) async fn stopped(&self) -> anyhow::Result<()> {
        ensure!(
            self.http
                .get(format!("{}/.well-known/jwks.json", self.endpoint))
                .send()
                .await
                .is_err(),
            "the owned identity listener remains after environment teardown"
        );
        Ok(())
    }
}
