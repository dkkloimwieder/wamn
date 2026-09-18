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
    human: String,
    ca: std::path::PathBuf,
    invitation: String,
    operator: reqwest::Client,
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
        let mut operator_pem = std::fs::read(
            environment
                .issuer
                .args
                .client_cert
                .as_ref()
                .context("operator certificate")?,
        )?;
        operator_pem.extend(std::fs::read(
            environment
                .issuer
                .args
                .client_key
                .as_ref()
                .context("operator key")?,
        )?);
        let operator = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca)?)
            .identity(reqwest::Identity::from_pem(&operator_pem)?)
            .build()?;
        let refused = http
            .post(format!("{endpoint}/invitations"))
            .json(&json!({"principal_id":human.id().as_str()}))
            .send()
            .await?;
        ensure!(
            refused.status() == reqwest::StatusCode::FORBIDDEN,
            "an unauthenticated operator could invite"
        );
        let response = operator
            .post(format!("{endpoint}/invitations"))
            .json(&json!({"principal_id":human.id().as_str()}))
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::CREATED,
            "operator invitation refused"
        );
        let mail: Value = serde_json::from_slice(&std::fs::read(
            std::env::var_os("WAMN_TEST_INVITATION_FILE")
                .context("run with the local invitation capture")?,
        )?)?;
        ensure!(mail["to"] == json!([EMAIL]), "invitation recipient differs");
        let body = mail["text"].as_str().context("invitation email text")?;
        ensure!(
            body.lines()
                .any(|line| line == format!("Principal: {}", human.id().as_str())),
            "invitation principal differs"
        );
        let invitation = body
            .lines()
            .find_map(|line| line.strip_prefix("Invitation secret: "))
            .context("invitation email secret")?
            .to_owned();
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
            human: human.id().as_str().to_owned(),
            ca: ca_path.clone(),
            invitation,
            operator,
        };
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

    pub(super) async fn terminal(
        &self,
        environment: &DevEnvironment,
        served: &super::local_delivery::Served,
    ) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt as _;
        let (project, task) = connect(&environment.route.database_url).await?;
        let actor = PlatformComponent::Provisioning.principal_id().to_string();
        project.execute("SELECT set_config('app.user_id', $1, false), set_config('app.tenant_id', $2, false), set_config('app.operation','admin:seed-identity-fixture',false)", &[&actor, &TENANT]).await?;
        project.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,'route-caller') ON CONFLICT DO NOTHING", &[&TENANT,&self.human]).await?;
        project.execute("INSERT INTO receiving.purchase_order (id,purchase_order_number,supplier_id) VALUES (gen_random_uuid(),'PASSWORD-JOURNEY','00000000-0000-0000-0000-000000000401')", &[]).await?;
        let repository = super::super::repository_root()?;
        let binary = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(|| repository.join("target"), std::path::PathBuf::from)
            .join("debug/wamn-receiving");
        let mut child = tokio::process::Command::new("python3")
            .arg(repository.join("apps/wamn_receiving/tests/password_receiving_pty.py"))
            .arg("--binary")
            .arg(binary)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        child
            .stdin
            .take()
            .context("private journey input")?
            .write_all(&serde_json::to_vec(&json!({
                "url":served.url,"host":served.host,"instance":served.instance,
                "issuer":self.endpoint,"audience":self.audience,"ca":self.ca,
                "email":EMAIL,"password":PASSWORD,"principal":self.human,"invitation":self.invitation,
            }))?)
            .await?;
        let output = child.wait_with_output().await?;
        ensure!(
            output.status.success(),
            "password terminal journey failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let replay = self.http.post(format!("{}/password/enroll", self.endpoint))
            .json(&json!({"principal_id":self.human,"invitation":self.invitation,"password":"a different disposable password"}))
            .send().await?;
        ensure!(
            replay.status() == reqwest::StatusCode::BAD_REQUEST,
            "the consumed invitation was accepted twice"
        );
        let repeat = self
            .operator
            .post(format!("{}/invitations", self.endpoint))
            .json(&json!({"principal_id":self.human}))
            .send()
            .await?;
        ensure!(
            repeat.status() == reqwest::StatusCode::BAD_REQUEST,
            "an enrolled account accepted a replacement invitation"
        );
        project.execute("DELETE FROM app_system.user_roles WHERE tenant_id=$1 AND user_id=$2::text::uuid AND role_name='route-caller'", &[&TENANT,&self.human]).await?;
        task.abort();
        let response = self
            .http
            .post(format!("{}/password/session", self.endpoint))
            .json(&json!({"email":EMAIL,"password":PASSWORD,"aud":self.audience}))
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "password session refused before permission test"
        );
        let credentials: Value = response.json().await?;
        let token = credentials["access_token"]
            .as_str()
            .context("password session token")?;
        let refused = reqwest::Client::new()
            .post(format!("{}/purchase_order/query", served.url))
            .header("Host", &served.host)
            .bearer_auth(token)
            .json(&json!([{"request_id":"password-permission-refusal"}]))
            .send()
            .await?;
        ensure!(
            refused.status() == reqwest::StatusCode::FORBIDDEN,
            "a session without an operation grant was accepted"
        );
        Ok(())
    }

    pub(super) async fn refuse_missing_membership(&self, system: &str) -> anyhow::Result<()> {
        let (admin, task) = connect(system).await?;
        let actor = PlatformComponent::Provisioning.principal_id().to_string();
        admin
            .execute("SELECT set_config('app.user_id', $1, false)", &[&actor])
            .await?;
        let removed = wamn_platform_identity::revoke_project_env_membership(
            &*admin,
            &self.human.parse()?,
            ORG,
            PROJECT,
            ENVIRONMENT,
        )
        .await?;
        task.abort();
        ensure!(removed, "the fixture had membership to revoke");
        let response = self
            .http
            .post(format!("{}/password/session", self.endpoint))
            .json(&json!({"email":EMAIL,"password":PASSWORD,"aud":self.audience}))
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::UNAUTHORIZED,
            "password login accepted missing membership"
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
