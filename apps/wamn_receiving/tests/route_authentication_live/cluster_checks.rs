//! Local checks of the deployed fixture helpers.
use super::case_execution::{validate_response, with_signals};
use super::*;
use std::fs;

#[tokio::test]
async fn cluster_startup_initializes_tls_client_provider() -> anyhow::Result<()> {
    let evidence = std::env::temp_dir().join(format!("receiving-tls-{}", uuid::Uuid::new_v4()));
    let result = with_signals(&evidence, async {
        let _client = async_nats::rustls::ClientConfig::builder()
            .with_root_certificates(async_nats::rustls::RootCertStore::empty())
            .with_no_client_auth();
        Ok(())
    })
    .await;
    fs::remove_dir_all(&evidence)?;
    result
}

#[tokio::test]
async fn failed_case_retains_tracing_and_identity_output() -> anyhow::Result<()> {
    let evidence = std::env::temp_dir().join(format!("receiving-logs-{}", uuid::Uuid::new_v4()));
    let error = with_signals(&evidence, async {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        fs::File::create(evidence.join("identity.log"))?;
        std::os::unix::fs::symlink(
            evidence.join("identity.log"),
            scratch.path().join("identity.log"),
        )?;
        fs::write(
            scratch.path().join("identity.log"),
            "identity output after readiness\n",
        )?;
        tracing::warn!("provisioning test marker");
        anyhow::bail!("forced provisioning failure")
    })
    .await
    .expect_err("the case must fail");
    let message = format!("{error:#}");
    assert!(message.contains("forced provisioning failure"));
    for name in ["identity.log", "test.log"] {
        assert!(message.contains(&evidence.join(name).display().to_string()));
    }
    assert_eq!(
        fs::read_to_string(evidence.join("identity.log"))?,
        "identity output after readiness\n"
    );
    let trace = fs::read_to_string(evidence.join("test.log"))?;
    assert!(trace.contains("provisioning test marker"));
    assert!(trace.contains("forced provisioning failure"));
    fs::remove_dir_all(evidence)?;
    Ok(())
}

#[test]
fn readiness_requires_the_released_route_response() {
    let body = br#"{"error":{"code":"route-not-found"}}"#;
    validate_response(404, "application/json", body).unwrap();
    assert!(validate_response(200, "application/json", body).is_err());
    assert!(validate_response(404, "text/html", b"<h1>Not Found</h1>").is_err());
    assert!(validate_response(404, "application/json", br#"{"error":{"code":"other"}}"#).is_err());
}

#[tokio::test]
async fn a_failed_command_keeps_its_stderr() {
    let error = super::case_execution::checked(
        tokio::process::Command::new("sh")
            .args(["-c", "echo 'Cannot pull supplied image' >&2; exit 3"]),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("sh failed with exit status: 3"), "{error}");
    assert!(error.contains("Cannot pull supplied image"), "{error}");
}
