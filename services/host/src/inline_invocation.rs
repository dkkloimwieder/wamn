use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use wamn_execution_host::{ExecutionHost, ExecutionIdentity, production_capabilities};
use wamn_runtime::flow_invocation::{InlineRunClaim, InlineRunDriver};
use wamn_runtime::plugins::{RunnerEgressPolicy, WamnCredentials, WamnLogging, WamnPostgres};
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;

pub struct InlineExecutionDriver {
    engine: Arc<Engine>,
    guest: Arc<[u8]>,
    postgres: Arc<WamnPostgres>,
    credentials: Arc<WamnCredentials>,
    logging: Arc<WamnLogging>,
    allowed_hosts: Arc<[AllowedHost]>,
    lease_ttl_ms: u64,
}

impl InlineExecutionDriver {
    pub fn new(
        engine: Arc<Engine>,
        flowrunner: &Path,
        postgres: Arc<WamnPostgres>,
        logging: Arc<WamnLogging>,
        credentials_file: Option<&Path>,
        allowed_hosts: Arc<[AllowedHost]>,
        lease_ttl_ms: u64,
    ) -> anyhow::Result<Self> {
        let guest = std::fs::read(flowrunner)
            .with_context(|| format!("read flowrunner component {}", flowrunner.display()))?;
        let credentials = match credentials_file {
            Some(path) => WamnCredentials::from_file(path)?,
            None => WamnCredentials::empty(),
        };
        Ok(Self {
            engine,
            guest: guest.into(),
            postgres,
            credentials: Arc::new(credentials),
            logging,
            allowed_hosts,
            lease_ttl_ms,
        })
    }
}

impl InlineRunDriver for InlineExecutionDriver {
    fn start(&self, claim: InlineRunClaim) -> anyhow::Result<()> {
        let engine = self.engine.clone();
        let guest = self.guest.clone();
        let postgres = self.postgres.clone();
        let credentials = self.credentials.clone();
        let logging = self.logging.clone();
        let allowed_hosts = self.allowed_hosts.clone();
        let lease_ttl_ms = self.lease_ttl_ms;
        tokio::spawn(async move {
            let result = async {
                let mut host = ExecutionHost::instantiate(
                    &engine,
                    &guest,
                    postgres,
                    credentials,
                    logging,
                    ExecutionIdentity {
                        owner: &claim.lease_owner,
                        tenant: &claim.tenant,
                        schema: claim.schema.as_deref(),
                        project: &claim.project,
                        org: None,
                        environment: None,
                        database: None,
                    },
                    production_capabilities(allowed_hosts, Arc::new(RunnerEgressPolicy::default())),
                    lease_ttl_ms,
                )
                .await?;
                host.execute_claimed(&claim.run_id, &claim.lease_owner, claim.lease_generation)
                    .await
            }
            .await;
            if let Err(error) = result {
                tracing::error!(
                    run_id = %claim.run_id,
                    lease_owner = %claim.lease_owner,
                    lease_generation = claim.lease_generation,
                    error = %error,
                    "inline invocation execution stopped; durable lease recovery remains authoritative"
                );
            }
        });
        Ok(())
    }
}
