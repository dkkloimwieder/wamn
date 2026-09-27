//! `wamn-ctl intents`: list and resolve the uncertain route intents of one
//! tenant and environment in `wamn_run.intents` (`wamn-an24`).
//!
//! The verbs match `wamn-edge intents`. An uncertain intent began and never
//! finished, so its item answers `intent-uncertain` until an operator resolves
//! it here. After that, its key answers `intent-resolved` with the basis, and
//! the caller sends a new key.

use anyhow::{Context as _, bail};
use clap::{Args, Subcommand};
use tokio_postgres::NoTls;
use wamn_run_state::intent_sql::{
    environment_resolve_intent_sql, environment_uncertain_intents_sql,
};
use wamn_run_state::operator_action::OperatorActionBasis;

use crate::workflow_verbs::WorkflowScope;

/// One intents verb.
#[derive(Debug, Subcommand)]
pub enum IntentCommand {
    /// Print the uncertain intents, one tab-separated line each: id,
    /// operation, key, package, release, tenant.
    List(ListArgs),
    /// Close one uncertain intent by an operator decision.
    Resolve(ResolveArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
}

#[derive(Debug, Args)]
pub struct ResolveArgs {
    #[command(flatten)]
    pub scope: WorkflowScope,
    pub id: i64,
    /// external-evidence, counterparty-confirmation or operator-judgment.
    pub basis: OperatorActionBasis,
}

/// Run one intents verb and print its result.
pub async fn run(command: IntentCommand) -> anyhow::Result<()> {
    let scope = match &command {
        IntentCommand::List(args) => &args.scope,
        IntentCommand::Resolve(args) => &args.scope,
    };
    let (mut client, connection) = tokio_postgres::connect(&scope.admin_database_url, NoTls)
        .await
        .context("connect to the project database")?;
    tokio::spawn(connection);
    let transaction = client.transaction().await?;
    transaction
        .execute(
            "SELECT set_config('search_path', $1, true), set_config('app.tenant', $2, true)",
            &[&scope.schema, &scope.tenant],
        )
        .await
        .context("bind the tenant")?;
    match &command {
        IntentCommand::List(_) => {
            let rows = transaction
                .query(&environment_uncertain_intents_sql(), &[&scope.environment])
                .await
                .context("list the uncertain intents")?;
            for row in rows {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    row.get::<_, i64>(0),
                    row.get::<_, String>(4),
                    row.get::<_, String>(5),
                    row.get::<_, String>(3),
                    row.get::<_, String>(2),
                    row.get::<_, String>(1),
                );
            }
        }
        IntentCommand::Resolve(args) => {
            let changed = transaction
                .execute(
                    &environment_resolve_intent_sql(),
                    &[&args.id, &args.basis.as_str(), &scope.environment],
                )
                .await
                .context("resolve the intent")?;
            if changed == 0 {
                bail!(
                    "intent {} is not uncertain in tenant {} environment {}",
                    args.id,
                    scope.tenant,
                    scope.environment
                );
            }
            println!("resolved intent {} by {}", args.id, args.basis);
        }
    }
    transaction.commit().await?;
    Ok(())
}
