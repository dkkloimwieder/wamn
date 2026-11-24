//! Candidate health without changes to connection facts or credentials.

use std::sync::Arc;

use crate::transport::HttpTransport;

/// Checks a candidate connection with the process HTTP transport.
pub struct ConnectionHealth {
    transport: Arc<HttpTransport>,
}

impl std::fmt::Debug for ConnectionHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionHealth").finish_non_exhaustive()
    }
}

impl ConnectionHealth {
    pub fn new(transport: Arc<HttpTransport>) -> Self {
        Self { transport }
    }

    /// Link candidate values and return only non-secret health diagnostics.
    pub async fn probe<F, Fut>(
        &self,
        integration_type: &str,
        runtime: &str,
        parameters: &serde_json::Value,
        auth_type: &str,
        credential: Option<&str>,
        storage_health: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        match runtime {
            "host" => match integration_type {
                "http" => crate::health(&self.transport, parameters, auth_type, credential)
                    .await
                    .map_err(|error| {
                        if matches!(error.parse::<u16>(), Ok(401 | 403 | 500..=599)) {
                            error
                        } else {
                            "HTTP health failed".to_owned()
                        }
                    }),
                "gcs" | "s3" => storage_health()
                    .await
                    .map_err(|_| "storage health failed".to_owned()),
                _ => Err("integration health unavailable".into()),
            },
            "wasm" | "native" => {
                Err("integration health unavailable outside an application workload".into())
            }
            _ => Err("integration health unavailable".into()),
        }
    }
}
