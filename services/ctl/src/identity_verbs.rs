//! Arguments and output of the `provision-identity-issuer`,
//! `grant-project-env-membership`, and `revoke-project-env-membership` verbs.

use std::fmt;
use std::path::PathBuf;

use clap::{ArgGroup, Args};
use wamn_control::identity_issuer::{
    IdentityIssuerAction, IdentityIssuerRequest, provision_identity_issuer,
};
use wamn_control::project_env_membership::{self, ProjectEnvMembershipRequest};
use wamn_control_provision::CredentialGeneration;
use wamn_platform_identity::PrincipalId;

/// Provisioning inputs for one identity authority, not a project environment.
#[derive(Args)]
#[command(group(ArgGroup::new("identity_generation_action").required(true).multiple(false)
    .args(["prepare_generation", "retire_generation", "abort_generation"])))]
pub struct IdentityIssuerArgs {
    /// Exact HTTPS issuer configured on wamn-identity.
    #[arg(long)]
    pub issuer: String,
    /// Administrator URL for the wamn_system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,
    /// Prepare an inactive A/B credential slot.
    #[arg(long, requires = "emit_secret")]
    pub prepare_generation: Option<CredentialGeneration>,
    /// Retire a slot after its replacement has a live session.
    #[arg(long)]
    pub retire_generation: Option<CredentialGeneration>,
    /// Abort an unused prepared slot that has no live sessions.
    #[arg(long)]
    pub abort_generation: Option<CredentialGeneration>,
    /// Write the prepared credential Secret atomically with mode 0600.
    #[arg(long, requires = "prepare_generation")]
    pub emit_secret: Option<PathBuf>,
    /// Namespace for the emitted Secret.
    #[arg(long, default_value = "wamn-system")]
    pub namespace: String,
    /// Name for the emitted Secret, matching the identity chart.
    #[arg(long, default_value = "wamn-identity-db")]
    pub secret_name: String,
}

impl fmt::Debug for IdentityIssuerArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityIssuerArgs")
            .field("system_database_url", &"[REDACTED]")
            .field("prepare_generation", &self.prepare_generation)
            .field("retire_generation", &self.retire_generation)
            .field("abort_generation", &self.abort_generation)
            .finish_non_exhaustive()
    }
}

/// Arguments that name one existing human and one existing project environment.
#[derive(Debug, Args)]
pub struct ProjectEnvMembershipArgs {
    /// Organization that owns the project environment.
    #[arg(long)]
    pub org: String,

    /// Project within the organization.
    #[arg(long)]
    pub project: String,

    /// Exact environment within the project.
    #[arg(long)]
    pub env: String,

    /// Existing human principal UUID from the system database.
    #[arg(long)]
    pub principal_id: String,

    /// Provisioning administrator URL for the system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,
}

/// Run one identity credential generation action and print what it did.
pub async fn provision_issuer(args: IdentityIssuerArgs) -> anyhow::Result<()> {
    let outcome = provision_identity_issuer(IdentityIssuerRequest {
        issuer: args.issuer,
        system_database_url: args.system_database_url,
        prepare_generation: args.prepare_generation,
        retire_generation: args.retire_generation,
        abort_generation: args.abort_generation,
        emit_secret: args.emit_secret,
        namespace: args.namespace,
        secret_name: args.secret_name,
    })
    .await?;
    println!(
        "{} identity database generation {}",
        match outcome.action {
            IdentityIssuerAction::Prepared => "prepared",
            IdentityIssuerAction::Retired => "retired",
            IdentityIssuerAction::Aborted => "aborted",
        },
        outcome.generation.as_str()
    );
    Ok(())
}

fn membership_request(args: &ProjectEnvMembershipArgs) -> ProjectEnvMembershipRequest {
    ProjectEnvMembershipRequest {
        org: args.org.clone(),
        project: args.project.clone(),
        env: args.env.clone(),
        principal_id: args.principal_id.clone(),
        system_database_url: args.system_database_url.clone(),
    }
}

fn print_membership(state: &str, args: &ProjectEnvMembershipArgs, principal_id: &PrincipalId) {
    println!(
        "membership {state} principal_id={principal_id} org={} project={} env={}",
        args.org, args.project, args.env
    );
}

/// Grant one human's project environment membership and print the new state.
pub async fn grant(args: ProjectEnvMembershipArgs) -> anyhow::Result<()> {
    let principal_id = project_env_membership::grant(membership_request(&args)).await?;
    print_membership("granted", &args, &principal_id);
    Ok(())
}

/// Revoke one human's project environment membership and print the new state.
pub async fn revoke(args: ProjectEnvMembershipArgs) -> anyhow::Result<()> {
    let principal_id = project_env_membership::revoke(membership_request(&args)).await?;
    print_membership("revoked", &args, &principal_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser};

    use super::{IdentityIssuerArgs, ProjectEnvMembershipArgs};

    #[derive(Parser)]
    struct IssuerCli {
        #[command(flatten)]
        args: IdentityIssuerArgs,
    }

    #[derive(Parser)]
    struct MembershipCli {
        #[command(flatten)]
        args: ProjectEnvMembershipArgs,
    }

    fn issuer_arguments() -> Vec<&'static str> {
        vec![
            "issuer",
            "--issuer",
            "https://identity.wamn-system.svc",
            "--system-database-url",
            "postgres://admin:hidden-value@sysdb/wamn_system",
            "--prepare-generation",
            "a",
            "--emit-secret",
            "identity.json",
        ]
    }

    fn membership_arguments() -> [&'static str; 11] {
        [
            "membership",
            "--org",
            "acme",
            "--project",
            "billing",
            "--env",
            "dev",
            "--principal-id",
            "00112233-4455-6677-8899-aabbccddeeff",
            "--system-database-url",
            "postgres://admin:secret@localhost/wamn_system",
        ]
    }

    #[test]
    fn scoped_prepare_arguments_parse_and_redact_admin_credentials() {
        let args = IssuerCli::try_parse_from(issuer_arguments()).unwrap().args;
        assert_eq!(args.namespace, "wamn-system");
        assert_eq!(args.secret_name, "wamn-identity-db");
        assert!(!format!("{args:?}").contains("hidden-value"));
    }

    #[test]
    fn actions_and_secret_output_are_not_ambiguous() {
        for extra in [
            vec!["--retire-generation", "b"],
            vec!["--abort-generation", "b"],
            vec!["--org", "acme"],
        ] {
            let mut values = issuer_arguments();
            values.extend(extra);
            assert!(IssuerCli::try_parse_from(values).is_err());
        }
        let mut values = issuer_arguments();
        values.truncate(7);
        assert!(IssuerCli::try_parse_from(values.iter().copied()).is_err());
        values.truncate(5);
        assert!(IssuerCli::try_parse_from(values.iter().copied()).is_err());
        values.extend(["--retire-generation", "a"]);
        assert!(IssuerCli::try_parse_from(values).is_ok());
    }

    #[test]
    fn exact_membership_arguments_parse() {
        let args = MembershipCli::try_parse_from(membership_arguments())
            .unwrap()
            .args;
        assert_eq!(args.org, "acme");
        assert_eq!(args.project, "billing");
        assert_eq!(args.env, "dev");
        assert_eq!(args.principal_id, "00112233-4455-6677-8899-aabbccddeeff");
    }

    #[test]
    fn every_membership_argument_is_required() {
        let command = MembershipCli::command();
        for name in [
            "org",
            "project",
            "env",
            "principal_id",
            "system_database_url",
        ] {
            let argument = command
                .get_arguments()
                .find(|argument| argument.get_id() == name)
                .unwrap();
            assert!(argument.is_required_set(), "{name} must be required");
        }
        let database = command
            .get_arguments()
            .find(|argument| argument.get_id() == "system_database_url")
            .unwrap();
        assert_eq!(database.get_env().unwrap(), "WAMN_SYSTEM_ADMIN_URL");
    }
}
