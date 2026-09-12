//! Prove human membership and permission revocation through the deployed Receiving HTTP route.
//!
//! The caller supplies an already provisioned disposable Receiving fixture.
//! Only this run's new identity, PAT, tenant user, and role are created and removed.

use std::fmt;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, anyhow, ensure};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_platform_identity::{PrincipalId, assign_project_role, create_human, issue_pat};

const PURCHASE_ORDER_ID: &str = "00000000-0000-0000-0000-000000000301";
const OPERATION_GRANT: &str = "wamn-receiving:purchase-order/get@1.0.0";
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Inputs for the existing disposable Receiving deployment.
#[derive(Args)]
pub struct MembershipTestArgs {
    /// Provisioning administrator URL for the existing system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,
    /// Administrator URL for the existing Receiving project database.
    #[arg(long, env = "WAMN_PROJECT_ADMIN_URL", hide_env_values = true)]
    pub project_database_url: String,
    /// Reachable base URL of the deployed Receiving route.
    #[arg(long)]
    pub endpoint_url: String,
    /// Released route's HTTP Host header.
    #[arg(long)]
    pub host: String,
    /// Organization that owns the disposable fixture.
    #[arg(long)]
    pub org: String,
    /// Project that owns the disposable fixture.
    #[arg(long)]
    pub project: String,
    /// Exact provisioned environment.
    #[arg(long)]
    pub env: String,
    /// Tenant already attached to the released route.
    #[arg(long)]
    pub tenant: String,
    /// Seed a disposable human benchmark fixture and write its PAT to a new mode-0600 file.
    /// The journey teardown owns these retained facts; this mode runs no HTTP proof.
    #[arg(long)]
    pub throughput_pat_file: Option<PathBuf>,
}

impl fmt::Debug for MembershipTestArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MembershipProofArgs")
            .finish_non_exhaustive()
    }
}

/// Run every real HTTP case and remove this run's identity and permission facts.
pub async fn run(args: MembershipTestArgs) -> anyhow::Result<()> {
    wamn_control_provision::validate_project_env(&args.org, &args.project, &args.env)
        .context("invalid membership proof scope")?;
    ensure!(!args.tenant.is_empty(), "membership proof needs a tenant");
    let http = reqwest::Client::builder()
        .timeout(OPERATION_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| anyhow!("build membership proof HTTP client"))?;
    let mut system = connect(&args.system_database_url, "system").await?;
    let mut project = connect(&args.project_database_url, "project").await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("assume identity provisioning authority")?;
    let nonce: String = system
        .query_one("SELECT gen_random_uuid()::text", &[])
        .await
        .context("allocate a unique proof identity")?
        .get(0);
    let subject = format!("membership-proof-{nonce}@example.test");
    let human = create_human(&system, &subject, "Disposable membership proof")
        .await
        .context("create the proof human")?;
    let role = format!("membership-proof-{nonce}");
    if let Some(path) = &args.throughput_pat_file {
        let result = tokio::time::timeout(TEST_TIMEOUT, async {
            seed_tenant_role(&args, &mut project, human.id(), &role).await?;
            membership(&args, human.id(), "grant-project-env-membership").await?;
            let token = issue_pat(
                &system,
                human.id(),
                "Disposable fresh-auth benchmark",
                Duration::from_secs(2 * 60 * 60),
            )
            .await
            .context("issue the two-hour benchmark PAT")?;
            write_benchmark_pat(path, token.token())
        })
        .await
        .map_err(|_| anyhow!("human benchmark fixture timed out"))
        .and_then(|result| result);
        if result.is_err() {
            cleanup(&mut system, &mut project, &args.tenant, human.id(), &role).await?;
        }
        result?;
        println!("MEMBERSHIP_BENCH fixture=ready credential=human-pat cleanup=journey");
        return Ok(());
    }
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        exercise(&args, &http, &system, &mut project, human.id(), &role),
    )
    .await
    .map_err(|_| anyhow!("membership proof exceeded its five-minute deadline"))
    .and_then(|result| result);
    let cleaned = tokio::time::timeout(
        TEST_TIMEOUT,
        cleanup(&mut system, &mut project, &args.tenant, human.id(), &role),
    )
    .await
    .map_err(|_| anyhow!("membership proof cleanup timed out"))
    .and_then(|result| result);
    if let Err(error) = cleaned {
        return Err(error).context(match result {
            Ok(()) => "HTTP cases passed, but proof cleanup failed",
            Err(_) => "HTTP proof and its cleanup failed",
        });
    }
    result?;
    println!("MEMBERSHIP_PROOF result=pass cases=7 cleanup=pass");
    Ok(())
}

fn write_benchmark_pat(path: &Path, token: &str) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("create private benchmark PAT file without replacing a file")?;
    file.write_all(token.as_bytes())
        .context("write private benchmark PAT file")
}

async fn connect(url: &str, name: &str) -> anyhow::Result<Client> {
    let (client, connection) =
        tokio::time::timeout(OPERATION_TIMEOUT, tokio_postgres::connect(url, NoTls))
            .await
            .map_err(|_| anyhow!("{name} database connection timed out"))?
            .map_err(|_| anyhow!("{name} database connection failed"))?;
    // Dropping the client ends this driver; connection details never enter output.
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute("SET statement_timeout = '30s'")
        .await
        .with_context(|| format!("bound {name} database operations"))?;
    Ok(client)
}

async fn exercise(
    args: &MembershipTestArgs,
    http: &reqwest::Client,
    system: &Client,
    project: &mut Client,
    principal: &PrincipalId,
    role: &str,
) -> anyhow::Result<()> {
    let token = issue_pat(
        system,
        principal,
        "Disposable membership proof",
        TEST_TIMEOUT,
    )
    .await
    .context("issue the proof PAT through production identity")?;
    assign_project_role(system, principal, &args.org, &args.project, "route-caller")
        .await
        .context("assign the project role that must not imply membership")?;
    seed_tenant_role(args, project, principal, role).await?;

    let request = |case: &'static str, status| {
        post_case(
            args,
            http,
            token.token(),
            format!("{principal}-{case}"),
            case,
            status,
        )
    };
    request("absent_membership", 401).await?;
    for case in ["granted", "repeated_grant"] {
        membership(args, principal, "grant-project-env-membership").await?;
        request(case, 200).await?;
    }
    let removed = project
        .execute(
            "DELETE FROM app_system.user_roles \
             WHERE tenant_id = $1 AND user_id = $2::text::uuid AND role_name = $3",
            &[&args.tenant, &principal.as_str(), &role],
        )
        .await
        .context("remove this human's permission-bearing role")?;
    ensure!(
        removed == 1,
        "proof role assignment was missing before removal"
    );
    request("role_removed", 403).await?;
    project
        .execute(
            "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
             VALUES ($1, $2::text::uuid, $3)",
            &[&args.tenant, &principal.as_str(), &role],
        )
        .await
        .context("restore this human's tenant role")?;
    request("role_restored", 200).await?;
    for case in ["revoked", "repeated_revoke"] {
        membership(args, principal, "revoke-project-env-membership").await?;
        request(case, 401).await?;
    }
    Ok(())
}

async fn seed_tenant_role(
    args: &MembershipTestArgs,
    project: &mut Client,
    principal: &PrincipalId,
    role: &str,
) -> anyhow::Result<()> {
    let tx = project
        .transaction()
        .await
        .context("begin proof fixture seed")?;
    tx.execute(
        "INSERT INTO app_system.users (tenant_id, id, email) \
         VALUES ($1, $2::text::uuid, $3)",
        &[
            &args.tenant,
            &principal.as_str(),
            &format!("{role}@example.test"),
        ],
    )
    .await
    .context("seed the exact platform-issued UUID in the tenant")?;
    tx.execute(
        "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2)",
        &[&args.tenant, &role],
    )
    .await
    .context("seed the dedicated proof role")?;
    tx.execute(
        "INSERT INTO app_system.permissions (tenant_id, role_name, permission) \
         VALUES ($1, $2, $3)",
        &[&args.tenant, &role, &OPERATION_GRANT],
    )
    .await
    .context("grant only the canonical purchase-order/get operation to the proof role")?;
    tx.execute(
        "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
         VALUES ($1, $2::text::uuid, $3)",
        &[&args.tenant, &principal.as_str(), &role],
    )
    .await
    .context("link the proof human to its tenant role")?;
    tx.commit().await.context("commit proof fixture seed")?;
    Ok(())
}

async fn membership(
    args: &MembershipTestArgs,
    principal: &PrincipalId,
    verb: &str,
) -> anyhow::Result<()> {
    let arguments = wamn_ctl::project_env_membership::ProjectEnvMembershipArgs {
        org: args.org.clone(),
        project: args.project.clone(),
        env: args.env.clone(),
        principal_id: principal.as_str().to_owned(),
        system_database_url: args.system_database_url.clone(),
    };
    tokio::time::timeout(OPERATION_TIMEOUT, async {
        match verb {
            "grant-project-env-membership" => wamn_ctl::project_env_membership::grant(arguments).await,
            "revoke-project-env-membership" => wamn_ctl::project_env_membership::revoke(arguments).await,
            _ => unreachable!("the membership test names only its two control operations"),
        }
    })
    .await
    .map_err(|_| anyhow!("{verb} timed out"))??;
    Ok(())
}

async fn post_case(
    args: &MembershipTestArgs,
    http: &reqwest::Client,
    token: &str,
    request_id: String,
    case: &str,
    expected: u16,
) -> anyhow::Result<()> {
    let response = http
        .post(format!(
            "{}/purchase_order/get",
            args.endpoint_url.trim_end_matches('/')
        ))
        .header(reqwest::header::HOST, &args.host)
        .bearer_auth(token)
        .json(&json!([{"request_id": request_id, "id": PURCHASE_ORDER_ID}]))
        .send()
        .await
        .map_err(|_| anyhow!("membership case {case}: HTTP request failed"))?;
    let status = response.status().as_u16();
    ensure!(
        status == expected,
        "membership case {case}: status={status}, expected={expected}"
    );
    if status == 200 {
        let body = response
            .json::<Value>()
            .await
            .map_err(|_| anyhow!("membership case {case}: invalid JSON response"))?;
        check_record(&body, &request_id).with_context(|| format!("membership case {case}"))?;
    }
    println!("MEMBERSHIP_PROOF case={case} status={status} result=pass");
    Ok(())
}

fn check_record(body: &Value, request_id: &str) -> anyhow::Result<()> {
    let items = body
        .as_array()
        .context("response must be a batched envelope")?;
    ensure!(items.len() == 1, "response must contain exactly one item");
    let item = &items[0];
    ensure!(
        item["request_id"] == request_id,
        "response lost request correlation"
    );
    ensure!(
        item.get("error").is_none(),
        "successful HTTP response contained an operation error"
    );
    ensure!(
        item["value"]["id"] == PURCHASE_ORDER_ID
            && item["value"]["purchase_order_number"] == "PO-301",
        "response did not return the seeded purchase order"
    );
    Ok(())
}

async fn cleanup(
    system: &mut Client,
    project: &mut Client,
    tenant: &str,
    principal: &PrincipalId,
    role: &str,
) -> anyhow::Result<()> {
    let project_result: anyhow::Result<()> = async {
        let tx = project.transaction().await?;
        tx.execute(
            "DELETE FROM app_system.users WHERE tenant_id = $1 AND id = $2::text::uuid",
            &[&tenant, &principal.as_str()],
        )
        .await?;
        tx.execute(
            "DELETE FROM app_system.roles WHERE tenant_id = $1 AND name = $2",
            &[&tenant, &role],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    let system_result: anyhow::Result<()> = async {
        let tx = system.transaction().await?;
        tx.execute(
            "DELETE FROM identity.pats WHERE principal_id = $1::text::uuid",
            &[&principal.as_str()],
        )
        .await?;
        // Cascades remove only this run's project role and membership facts.
        tx.execute(
            "DELETE FROM identity.principals WHERE id = $1::text::uuid",
            &[&principal.as_str()],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    project_result.context("remove proof tenant user and role")?;
    system_result.context("remove proof identity and PAT")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        MembershipTestArgs, OPERATION_GRANT, PURCHASE_ORDER_ID, check_record, write_benchmark_pat,
    };
    use clap::Parser;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn benchmark_pat_file_is_private_and_never_overwrites() {
        let path =
            std::env::temp_dir().join(format!("wamn-benchmark-pat-{}", uuid::Uuid::new_v4()));
        write_benchmark_pat(&path, "test-only-token").expect("write private token");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let refused = write_benchmark_pat(&path, "replacement").is_err();
        let contents = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).expect("remove this test's token file");
        assert_eq!(mode, 0o600);
        assert!(refused);
        assert_eq!(contents, "test-only-token");
    }

    #[derive(Parser)]
    struct TestCommand {
        #[command(flatten)]
        args: MembershipTestArgs,
    }

    #[test]
    fn permission_uses_the_generated_canonical_operation_grant() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../apps/wamn_receiving/generated/contracts/purchase_order/get.operation.json"
        ))
        .expect("parse the generated purchase-order/get contract");
        assert_eq!(contract["grant"], OPERATION_GRANT);
        assert_ne!(contract["permission_token"], OPERATION_GRANT);
    }

    #[test]
    fn argument_diagnostics_do_not_disclose_database_credentials() {
        let command = TestCommand::try_parse_from([
            "membershipproof",
            "--system-database-url",
            "postgres://proof:system-secret@system/proof",
            "--project-database-url",
            "postgres://proof:project-secret@project/proof",
            "--endpoint-url",
            "http://endpoint-secret",
            "--host",
            "receiving.example.test",
            "--org",
            "acme",
            "--project",
            "receiving",
            "--env",
            "dev",
            "--tenant",
            "fixture",
        ])
        .expect("parse every required proof argument");
        let diagnostic = format!("{:?}", command.args);
        for secret in ["system-secret", "project-secret", "endpoint-secret"] {
            assert!(!diagnostic.contains(secret));
        }
    }

    #[test]
    fn success_requires_the_correlated_seeded_record() {
        let good = json!([{"request_id": "proof", "value": {
            "id": PURCHASE_ORDER_ID, "purchase_order_number": "PO-301"
        }}]);
        check_record(&good, "proof").expect("the expected record passes");
        assert!(check_record(&good, "another-request").is_err());
        for bad in [
            json!([]),
            json!([good[0], good[0]]),
            json!([{"request_id": "proof", "error": "permission-denied", "value": good[0]["value"]}]),
            json!([{"request_id": "proof", "value": {"id": "another-order", "purchase_order_number": "PO-301"}}]),
            json!([{"request_id": "proof", "value": {"id": PURCHASE_ORDER_ID, "purchase_order_number": "PO-302"}}]),
        ] {
            assert!(check_record(&bad, "proof").is_err());
        }
    }
}
