//! The existing control-test constructor enforces delivery fixture ownership.

use std::net::{Ipv4Addr, TcpListener};
use std::process::Command;

#[path = "../../../services/ctl/tests/support/mod.rs"]
mod support;

#[test]
fn ctl_constructor_requires_owned_inputs_for_delivery() {
    const PROBE: &str = "WAMN_POSTGRES_CONSTRUCTOR_PROBE";
    if let Ok(probe) = std::env::var(PROBE) {
        let value = support::LockedUrl::optional();
        assert!(probe == "legacy" && value.is_none());
        return;
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    for (probe, url) in [
        ("legacy", None),
        ("required", None),
        (
            "unowned",
            Some(format!(
                "postgresql://postgres@127.0.0.1:{}/interactive",
                listener.local_addr().unwrap().port()
            )),
        ),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "ctl_constructor_requires_owned_inputs_for_delivery",
                "--nocapture",
            ])
            .env(PROBE, probe)
            .env_remove("WAMN_CTL_PG_URL")
            .env_remove(wamn_test_infrastructure::postgres::OWNERSHIP_ENV)
            .env_remove(wamn_test_infrastructure::postgres::REQUIRED_ENV);
        if probe != "legacy" {
            command.env(wamn_test_infrastructure::postgres::REQUIRED_ENV, "1");
        }
        if let Some(url) = url {
            command.env("WAMN_CTL_PG_URL", url);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.success(), probe == "legacy");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if probe == "required" {
            assert!(stderr.contains("requires WAMN_CTL_PG_URL"), "{stderr}");
        }
        if probe == "unowned" {
            assert!(stderr.contains("ownership record is required"), "{stderr}");
        }
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
