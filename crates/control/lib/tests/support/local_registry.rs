//! A local `registry:2` container for the live tests that push package
//! artifacts.

use std::process::Command;
use std::time::Duration;

/// A `registry:2` container, removed on drop.
pub struct LocalRegistry {
    name: String,
    /// `127.0.0.1:<port>` of the registry.
    pub address: String,
}

impl LocalRegistry {
    pub fn start(label: &str) -> Self {
        let name = format!("wamn-package-artifact-live-{label}-{}", std::process::id());
        let run = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-p",
                "127.0.0.1::5000",
                "registry:2",
            ])
            .output()
            .expect("run docker");
        assert!(
            run.status.success(),
            "start registry:2: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        // The guard exists from here, so a failed port read still removes it.
        let mut registry = Self {
            name,
            address: String::new(),
        };
        let port = Command::new("docker")
            .args(["port", &registry.name, "5000/tcp"])
            .output()
            .expect("run docker port");
        String::from_utf8_lossy(&port.stdout)
            .lines()
            .next()
            .expect("registry:2 publishes its port")
            .trim()
            .clone_into(&mut registry.address);
        registry
    }

    pub async fn ready(&self) {
        let url = format!("http://{}/v2/", self.address);
        for _ in 0..100 {
            if reqwest::get(&url)
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("registry:2 at {} did not answer", self.address);
    }
}

impl Drop for LocalRegistry {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
    }
}
