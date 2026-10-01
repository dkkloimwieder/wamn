//! Issue a management-author PAT on a disposable stack, for a dry run of
//! docs/plan/kind-to-type.md §3.2 B8 step 3.
//!
//! An operator mints through the `mint-pat` Job of docs/operations/gcp.md
//! §3.16. A disposable stack has no such Job, so this starts the temporary
//! `wamn-identity` of `wamn dev up` (`wamn_control::dev::pat_issuer`), issues
//! the PAT through it in the same process, and stops it. The issuer's
//! certificates live in its own private directory, which `stop` removes.
//!
//! WAMN_SYSTEM_ADMIN_URL=<superuser URL of wamn_system> \
//! WAMN_IDENTITY_BINARY=<wamn-identity executable> \
//! cargo run -p wamn-test-infrastructure --example pat_for_disposable_stack -- \
//!   <org> <project> <env> <tenant> <namespace> <PAT Secret file>

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use wamn_control::dev::pat_issuer;
use wamn_control::provision_project_env::{self, ProvisionProjectEnvRequest};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 6,
        "usage: <org> <project> <env> <tenant> <namespace> <PAT Secret file>"
    );
    let system_url = std::env::var("WAMN_SYSTEM_ADMIN_URL").context("set WAMN_SYSTEM_ADMIN_URL")?;
    let output = PathBuf::from(&arguments[5]);
    let directory = output
        .parent()
        .context("the PAT Secret file names its private directory")?;
    let issuer = pat_issuer::start(&system_url, directory).await?;
    let request = ProvisionProjectEnvRequest {
        org: arguments[0].clone(),
        project: arguments[1].clone(),
        env: arguments[2].clone(),
        tenant: Some(arguments[3].clone()),
        disposable: false,
        system_database_url: Some(system_url),
        cluster: None,
        connection_limit: None,
        // The default of the verb; a run that only issues a PAT renders no Database.
        cluster_namespace: "wamn-system".to_owned(),
        namespace: arguments[4].clone(),
        secret_namespace: None,
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        pat_issuer: issuer.args.clone(),
        emit_management_author_pat_secret: Some(output),
        emit_operator_pat_secret: None,
    };
    let pat = provision_project_env::provision_project_env(&request).await;
    let stopped = issuer.stop().await;
    pat.context("issue the management-author PAT")?;
    stopped.context("stop the disposable identity process")
}
