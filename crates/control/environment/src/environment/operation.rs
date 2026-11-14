//! Native execution of server-authorized application operations.

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use super::{Platform, document::EnvironmentDocument, sql};

/// Run one published schema operation through the existing SQL owners.
///
/// # Errors
/// Returns preparation, SQL, or reset failures without changing code selections.
pub async fn schema(
    platform: &Platform,
    triple: &wamn_registry::Triple,
    application: &str,
    release: Option<&str>,
    apply: bool,

    administrator: Option<&str>,
) -> anyhow::Result<sql::schema::Diff> {
    let document = EnvironmentDocument::read_serving(platform, triple).await?;
    let (release, candidates) =
        sql::schema::release_candidates(platform, &document, application, release).await?;
    let state = sql::schema::state(platform, &document).await?;
    let diff =
        sql::schema::plan(&document, application, Some(release), &candidates, &state).await?;
    if diff.mode == super::document::SchemaMode::Reset && diff.disposable == Some(true) {
        sql::schema::reset_candidates(platform, &diff, candidates).await?;
    }
    if apply {
        let release = diff
            .release
            .as_deref()
            .context("schema apply needs a published release")?;
        let administrator =
            administrator.context("schema apply requires the owning database credential")?;
        if diff.mode == super::document::SchemaMode::Reset {
            Box::pin(sql::reset::reset_deployed_with_admin(
                platform,
                triple,
                Some((application, release)),
                administrator,
            ))
            .await?;
        } else {
            let expected = sql::ExpectedDatabase::read(platform, &document).await?;
            let lock = expected.lock(platform, &document, false).await?;
            let result = lock
                .protect(async {
                    sql::schema::apply(platform, &document, application, release).await?;
                    super::ensure::ensure_cdc_inputs(platform, &document, Some(administrator)).await
                })
                .await;
            lock.release().await;
            result?;
        }
    }
    Ok(diff)
}

/// A server-authorized operation. No request selects a path or credential.
#[derive(Debug)]
pub struct Request<'a> {
    pub operation: &'a str,
    pub org: &'a str,
    pub project: &'a str,
    pub env: &'a str,
    pub application: Option<&'a str>,
    pub release: Option<&'a str>,
}

/// Execute one operation with platform configuration and credentials supplied by control.
///
/// # Errors
/// Returns invalid target, authorization input, SQL, Kubernetes, and rollout failures.
pub async fn execute(
    platform: &Platform,
    request: Request<'_>,
    actor: &wamn_identity_protocol::PrincipalId,
    administrator: Option<&str>,
) -> anyhow::Result<Value> {
    ensure!(
        matches!(
            request.operation,
            "deploy" | "rollback" | "stop" | "diff" | "apply" | "reset"
        ),
        "unknown application operation"
    );
    wamn_control_provision::validate_project_env(request.org, request.project, request.env)?;
    let triple = wamn_registry::Triple::new(request.org, request.env, request.project);
    let actor = actor.to_string();
    match request.operation {
        "deploy" => {
            let analysis = super::apply_selection(
                platform,
                &triple,
                request
                    .application
                    .context("deploy requires an application")?,
                super::document::ReleaseChange::Deploy(
                    request.release.context("deploy requires a release")?,
                ),
                Some(&actor),
            )
            .await?;
            Ok(json!({"notices":analysis.notices,"plan":analysis.plan}))
        }
        "rollback" => {
            let analysis = super::apply_selection(
                platform,
                &triple,
                request
                    .application
                    .context("rollback requires an application")?,
                super::document::ReleaseChange::Rollback(request.release),
                Some(&actor),
            )
            .await?;
            Ok(json!({"notices":analysis.notices,"plan":analysis.plan}))
        }
        "stop" => {
            let analysis = super::apply_selection(
                platform,
                &triple,
                request
                    .application
                    .context("stop requires an application")?,
                super::document::ReleaseChange::Stop,
                Some(&actor),
            )
            .await?;
            Ok(json!({"notices":analysis.notices,"plan":analysis.plan}))
        }
        "diff" | "apply" => {
            let diff = schema(
                platform,
                &triple,
                request
                    .application
                    .context("schema operation requires an application")?,
                request.release,
                request.operation == "apply",
                administrator,
            )
            .await?;
            Ok(serde_json::to_value([diff])?)
        }
        "reset" => {
            Box::pin(sql::reset::reset_deployed_with_admin(
                platform,
                &triple,
                None,
                administrator.context("reset administrator")?,
            ))
            .await
        }
        _ => unreachable!("operation was validated before execution"),
    }
}
