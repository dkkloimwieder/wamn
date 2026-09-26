//! Arguments and output of the `author-wiring`, `publish-release`, `promote`,
//! `reconcile-run-plane`, and `terminalize-effect-uncertain` verbs.

use std::path::PathBuf;

use clap::Args;
use wamn_catalog::PackageCoordinate;
use wamn_control::author_wiring::{self, AuthorWiringDocumentRequest};
use wamn_control::promote::{self, PromoteRequest};
use wamn_control::publish_release::{
    self, PublishReleaseRequest, ReleaseWiringTarget, parse_package,
};
use wamn_control::reconcile_run_plane::{self, ReconcileRunPlaneOutcome, ReconcileRunPlaneRequest};
use wamn_control::terminalize_effect_uncertain::{self, TerminalizeEffectUncertainRequest};
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

/// Arguments for the wiring-authorship verb.
#[derive(Debug, Args)]
pub struct AuthorWiringArgs {
    /// Owner URL to the project-environment database holding the catalog facts.
    #[arg(long)]
    pub database_url: String,

    /// Owner URL to the CONTROL database holding `wamn_run.gate_reports`.
    ///
    /// A separate URL because the report is a separate plane's fact: it is not
    /// in `catalog.wirings` and never was after wamn-0h0g.8.5.6. Pointing this
    /// at the project database refuses rather than passing — the relation is
    /// not there.
    #[arg(long)]
    pub control_database_url: String,

    /// Tenant claim carried by the authored wiring.
    #[arg(long)]
    pub tenant: String,

    /// Package identity the wiring is authored into.
    #[arg(long)]
    pub package_id: String,

    /// Exact package version whose component facts gate this wiring.
    #[arg(long)]
    pub package_version: String,

    /// The wiring document to submit; it carries its own id and version.
    #[arg(long)]
    pub wiring_document: PathBuf,
}

#[derive(Debug, Args)]
pub struct PublishReleaseArgs {
    #[arg(long)]
    pub database_url: String,
    #[arg(long)]
    pub control_database_url: String,
    #[arg(long)]
    pub org: String,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub effective_release_id: u32,
    #[arg(long)]
    pub environment: String,
    /// Principal already authenticated by the publication boundary.
    #[arg(long)]
    pub verified_publisher_principal: String,
    #[arg(long)]
    pub run_schema: String,
    /// Exact package membership; repeat once per package.
    #[arg(long = "package", value_parser = parse_package, required = true)]
    pub packages: Vec<PackageCoordinate>,
    /// Exact package-owned wiring; repeat once per wiring. A release whose
    /// attachments all target routes names none.
    #[arg(long = "wiring", value_name = "PACKAGE@VERSION::WIRING=VERSION")]
    pub wirings: Vec<ReleaseWiringTarget>,
    /// Package-owned attachment documents; repeat once per package.
    #[arg(long = "attachments", value_name = "PATH", required = true)]
    pub attachments: Vec<PathBuf>,
    /// Deployment-owned hostname applied to every HTTP route.
    #[arg(long)]
    pub route_host: Option<String>,
    /// Exact `wamn.json` for every package in the release.
    #[arg(long = "package-manifest", value_name = "PATH", required = true)]
    pub package_manifests: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PromoteArgs {
    #[arg(long)]
    pub source_database_url: String,
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub target_database_url: String,
    #[arg(long)]
    pub control_database_url: String,
    #[arg(long)]
    pub org: String,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub source_effective_release_id: u32,
    #[arg(long)]
    pub target_effective_release_id: u32,
    #[arg(long)]
    pub source_environment: String,
    #[arg(long)]
    pub target_environment: String,
    #[arg(long)]
    pub run_schema: String,
    #[arg(long)]
    pub artifact_base: String,
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,
    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,
    #[arg(long)]
    pub principal: String,
    #[arg(long, default_value = "promote-release")]
    pub reason: String,
}

#[derive(Debug, Args)]
pub struct ReconcileRunPlaneArgs {
    /// Administrative Postgres URL to the system registry. The reconciler reads
    /// the project-env's stored instance suffix here before resolving policy;
    /// admission never connects to this database. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Administrative Postgres URL to the exact registry-derived project
    /// database. Observation and apply require SUPERUSER or BYPASSRLS so
    /// forced-RLS legacy rows cannot be skipped. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Registry organization owning the environment policy.
    #[arg(long)]
    pub org: String,

    /// Registry project owning the exact provisioned database target.
    #[arg(long)]
    pub project: String,

    /// Tenant whose project-local policy row is converged.
    #[arg(long)]
    pub tenant: String,

    /// Environment policy name in the owning organization's registry set.
    #[arg(long)]
    pub env: String,

    /// The project-env schema the run-plane tables live in (e.g.
    /// `wamn_runner_demo`, `poc_f1`).
    #[arg(long)]
    pub schema: String,

    /// Print the reconcile plan without applying it (strictly read-only).
    #[arg(long)]
    pub dry_run: bool,
}

/// Exact operator input; effect identity and asserted outcome are absent by construction.
#[derive(Debug, Args)]
pub struct TerminalizeEffectUncertainArgs {
    /// Project-admin PostgreSQL URL for the project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Project run-plane schema.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,

    /// Exact tenant owning the run.
    #[arg(long)]
    pub tenant: String,

    /// Exact effect-uncertain run.
    #[arg(long)]
    pub run: String,

    /// Evidence basis: external-evidence, counterparty-confirmation, or operator-judgment.
    #[arg(long)]
    pub basis: OperatorActionBasis,

    /// Opaque non-empty reference to the evidence used.
    #[arg(long)]
    pub evidence_ref: String,

    /// Opaque non-empty idempotency correlation.
    #[arg(long)]
    pub correlation_id: String,
}

/// Author one gated wiring version and print its definition hash.
pub async fn author(args: AuthorWiringArgs) -> anyhow::Result<()> {
    let hash = author_wiring::author_wiring_document(AuthorWiringDocumentRequest {
        database_url: args.database_url,
        control_database_url: args.control_database_url,
        tenant: args.tenant,
        package_id: args.package_id,
        package_version: args.package_version,
        wiring_document: args.wiring_document,
    })
    .await?;
    println!("{hash}");
    Ok(())
}

/// Mint one effective release and print its manifest digest.
pub async fn publish(args: PublishReleaseArgs) -> anyhow::Result<()> {
    let digest = publish_release::publish_release(PublishReleaseRequest {
        database_url: args.database_url,
        control_database_url: args.control_database_url,
        org: args.org,
        project: args.project,
        tenant: args.tenant,
        effective_release_id: args.effective_release_id,
        environment: args.environment,
        verified_publisher_principal: args.verified_publisher_principal,
        run_schema: args.run_schema,
        packages: args.packages,
        wirings: args.wirings,
        attachments: args.attachments,
        route_host: args.route_host,
        package_manifests: args.package_manifests,
    })
    .await?;
    println!("{digest}");
    Ok(())
}

/// Promote one verified release and print the promotion line.
pub async fn promote(args: PromoteArgs) -> anyhow::Result<()> {
    let source = format!(
        "{}:{}",
        args.source_environment, args.source_effective_release_id
    );
    let target = format!(
        "{}:{}",
        args.target_environment, args.target_effective_release_id
    );
    let outcome = promote::promote(PromoteRequest {
        source_database_url: args.source_database_url,
        target_database_url: args.target_database_url,
        control_database_url: args.control_database_url,
        org: args.org,
        project: args.project,
        tenant: args.tenant,
        source_effective_release_id: args.source_effective_release_id,
        target_effective_release_id: args.target_effective_release_id,
        source_environment: args.source_environment,
        target_environment: args.target_environment,
        run_schema: args.run_schema,
        artifact_base: args.artifact_base,
        registry_auth_file: args.registry_auth_file,
        insecure_registry: args.insecure_registry,
        oci_ca_paths: args.oci_ca_paths,
        principal: args.principal,
        reason: args.reason,
    })
    .await?;
    println!(
        "promoted {} from {source} to {target} as {} ({} component artifact(s) verified, {} pointer flip(s))",
        outcome.source_manifest_digest,
        outcome.target_manifest_digest,
        outcome.verified_components,
        outcome.activated_wirings,
    );
    Ok(())
}

/// Reconcile one run plane and print its plan and policy lines.
pub async fn reconcile(args: ReconcileRunPlaneArgs) -> anyhow::Result<()> {
    let request = ReconcileRunPlaneRequest {
        system_database_url: args.system_database_url,
        admin_database_url: args.admin_database_url,
        org: args.org,
        project: args.project,
        tenant: args.tenant.clone(),
        env: args.env.clone(),
        schema: args.schema,
        dry_run: args.dry_run,
    };
    let outcome = reconcile_run_plane::reconcile_run_plane(request).await?;
    print_reconciled(&outcome, args.dry_run, &args.tenant, &args.env);
    Ok(())
}

/// Print the lines that report one run-plane reconciliation.
pub fn print_reconciled(
    outcome: &ReconcileRunPlaneOutcome,
    dry_run: bool,
    tenant: &str,
    env: &str,
) {
    let plan = &outcome.plan;
    let verb = if dry_run { "would apply" } else { "applied" };
    if plan.is_noop() {
        println!(
            "run plane already at the schema of record — no actions ({} tables at target)",
            plan.at_target.len()
        );
    } else {
        for a in &plan.actions {
            println!("{verb} {:?}: {}", a.kind, a.target);
        }
    }
    for (table, col) in &plan.extra_columns {
        println!("  [extra] {table}.{col} is not in the schema of record — left untouched");
    }
    if outcome.policy_changed {
        let mode = if dry_run {
            "would converge"
        } else {
            "converged"
        };
        println!(
            "  {mode} environment policy tenant={tenant:?} environment={env:?} durability_class={}",
            outcome.durability_class.as_sql(),
        );
    }
    let identity = &outcome.tenant_identity;
    if identity.app_schema_installed {
        println!("  {verb} app_system schema tenant={tenant:?}");
    }
    if identity.platform_rows_written > 0
        || identity.service_rows_written > 0
        || identity.person_rows_written > 0
    {
        println!(
            "  {verb} identity rows tenant={tenant:?} platform={} service={} person={}",
            identity.platform_rows_written,
            identity.service_rows_written,
            identity.person_rows_written,
        );
    }
}

/// Terminalize one effect-uncertain run and print its result.
pub async fn terminalize(args: TerminalizeEffectUncertainArgs) -> anyhow::Result<()> {
    let result = terminalize_effect_uncertain::terminalize_effect_uncertain(
        TerminalizeEffectUncertainRequest {
            admin_database_url: args.admin_database_url,
            schema: args.schema,
            tenant: args.tenant,
            run: args.run,
            basis: args.basis,
            evidence_ref: args.evidence_ref,
            correlation_id: args.correlation_id,
        },
    )
    .await?;
    println!("{result}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct AuthorProbe {
        #[command(flatten)]
        args: AuthorWiringArgs,
    }

    const COORDINATE: [&str; 10] = [
        "--database-url",
        "postgres://author.invalid/env",
        "--control-database-url",
        "postgres://author.invalid/control",
        "--tenant",
        "tenant-a",
        "--package-id",
        "orders",
        "--package-version",
        "3.0.0",
    ];

    fn parse(submission: &[&str]) -> Result<AuthorWiringArgs, clap::Error> {
        let mut argv = vec!["author-wiring"];
        argv.extend_from_slice(&COORDINATE);
        argv.extend_from_slice(submission);
        AuthorProbe::try_parse_from(argv).map(|probe| probe.args)
    }

    /// The document is the WHOLE submission, and argv adds nothing to it.
    ///
    /// The gate-report argument this used to require is gone (wamn-0h0g.8.5.6):
    /// the report keys on the wiring hash the document itself determines, so
    /// there is no report id left for a caller to supply or mis-supply. What
    /// argv still carries is WHERE to read that report — a database URL, not an
    /// identity — and it is required, so no invocation can omit the check.
    #[test]
    fn the_document_is_the_whole_artifact_and_argv_restates_nothing() {
        let complete =
            parse(&["--wiring-document", "wiring.json"]).expect("the submission surface parses");
        assert_eq!(complete.wiring_document, PathBuf::from("wiring.json"));
        assert_eq!(
            complete.control_database_url,
            "postgres://author.invalid/control"
        );

        // The control store is not optional: drop its URL and the verb cannot
        // be invoked at all, so there is no ungated authoring invocation.
        let mut without_control = vec!["author-wiring"];
        without_control.extend_from_slice(&COORDINATE[..2]);
        without_control.extend_from_slice(&COORDINATE[4..]);
        without_control.extend_from_slice(&["--wiring-document", "wiring.json"]);
        assert!(
            AuthorProbe::try_parse_from(without_control).is_err(),
            "authoring parsed with no control store to read the gate report from"
        );

        let refusals: [Vec<&str>; 3] = [
            // There is no artifact to submit.
            vec![],
            // The retired report argument is REFUSED, not ignored: a caller
            // still passing it is asking for an identity that no longer exists,
            // and silently accepting it would suggest it still meant something.
            vec![
                "--wiring-document",
                "wiring.json",
                "--gate-report-id",
                "gate-2026-08-23",
            ],
            // The wiring id and version are the document's; argv cannot restate
            // them, so a second authoring grammar cannot start here.
            vec!["--wiring-document", "wiring.json", "--wiring-id", "orders"],
        ];
        for refused in refusals {
            assert!(parse(&refused).is_err(), "accepted {refused:?}");
        }
    }
}
