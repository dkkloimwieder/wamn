//! Actual identity CLI and HTTPS routes on a disposable test DB.

use std::time::Duration;

use clap::Parser as _;
use tokio::process::Command;
use wamn_identity_service::IdentityConfig;
use wamn_identity_service::cli::Cli;
use wamn_provision::identity_role::identity_issuer_login;

const ISSUER: &str = "https://identity.service.internal";
const PASSWORD: &str = "identity-surface-fixture-secret";

#[test]
fn cli_and_validated_configuration_debug_do_not_disclose_database_credentials() {
    let role = identity_issuer_login(ISSUER).expect("targeted role");
    let url = format!("postgres://{role}:{PASSWORD}@127.0.0.1/wamn_system");
    for args in [
        vec![
            "wamn-identity",
            "--issuer",
            ISSUER,
            "--database-url",
            &url,
            "publish",
        ],
        vec![
            "wamn-identity",
            "publish",
            "--issuer",
            ISSUER,
            "--database-url",
            &url,
        ],
        vec![
            "wamn-identity",
            "serve",
            "--issuer",
            ISSUER,
            "--database-url",
            &url,
            "--tls-cert",
            "/mounted/tls.crt",
            "--tls-key",
            "/mounted/tls.key",
        ],
    ] {
        let cli = Cli::try_parse_from(args).expect("CLI contract");
        assert!(!format!("{cli:?}").contains(PASSWORD));
        assert!(!format!("{cli:?}").contains(&url));
    }
    let config = IdentityConfig::new(ISSUER, &url).expect("targeted configuration");
    assert!(!format!("{config:?}").contains(PASSWORD));
    assert!(!format!("{config:?}").contains(&url));
    let refused = format!("postgres://wamn_system:{PASSWORD}@unreachable.invalid/wamn_system");
    let error = IdentityConfig::new(ISSUER, &refused).expect_err("owner refused before any socket");
    assert!(!format!("{error:?} {error}").contains(PASSWORD));
    assert!(!format!("{error:?} {error}").contains(&refused));
}

#[tokio::test]
async fn dotenv_loading_precedence_and_secret_redaction() {
    use wamn_test_infrastructure::{process_input, scratch::ScratchRoot};
    let directory = ScratchRoot::create().unwrap();
    let input = directory.path().join("certificate.pem");
    process_input::create(&input).unwrap();
    let role = identity_issuer_login(ISSUER).unwrap();
    let url = format!("postgres://{role}:fixture@127.0.0.1/wamn_system");
    std::fs::write(
        directory.path().join(".env"),
        "RESEND_API_KEY='fixture secret'\nRESEND_FROM='WAMN <fixture@example.invalid>'\n",
    )
    .unwrap();
    for (key, flag, malformed, reads_certificate) in [
        (None, None, false, false),
        (Some("fixture-key"), None, false, true),
        (Some("fixture-key"), Some("fixture secret"), false, false),
        (None, None, true, false),
    ] {
        if malformed {
            std::fs::write(
                directory.path().join(".env"),
                "RESEND_API_KEY='unterminated-secret",
            )
            .unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_wamn-identity"));
        command
            .current_dir(directory.path())
            .env_clear()
            .env("TOKIO_WORKER_THREADS", "2")
            .env("TMPDIR", directory.path())
            .args([
                "--issuer",
                ISSUER,
                "--database-url",
                &url,
                "serve",
                "--tls-cert",
            ])
            .arg(&input)
            .args(["--tls-key", "missing.key"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(key) = key {
            command.env("RESEND_API_KEY", key);
        }
        if let Some(flag) = flag {
            command.args(["--resend-api-key", flag]);
        }
        let mut child = command.spawn().unwrap();
        let writer = process_input::reader(&input, &mut child).await.unwrap();
        assert_eq!(writer.is_some(), reads_certificate);
        drop(writer);
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        for secret in ["fixture secret", "fixture-key", "unterminated-secret"] {
            assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
            assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        }
    }
}
