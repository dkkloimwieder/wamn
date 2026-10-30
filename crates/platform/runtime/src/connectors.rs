//! Pull and prepare application connectors while their host keeps serving.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use oci_client::secrets::RegistryAuth;
use tokio::sync::Mutex;
use wamn_engine::connector::{Connector, ConnectorBinding, ConnectorReader};
use wamn_package_artifact::registry_credentials::RegistryCredentialSource;
use wamn_package_artifact::{PackageRegistry, Repository};

use crate::connection_credentials::ConnectionCredentials;

/// Poll bindings every two seconds when no binding-change signal is available.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pin {
    digest: String,
    connection: String,
    generation: i64,
}

#[derive(Debug)]
struct Retry {
    after: Instant,
    delay: Duration,
    error: String,
}

#[derive(Debug)]
struct PinState {
    pin: Pin,
    retry: Option<Retry>,
}

impl PinState {
    fn ready(&self, pin: &Pin, now: Instant) -> bool {
        self.pin != *pin || self.retry.as_ref().is_some_and(|retry| now >= retry.after)
    }

    /// Record the next attempt and report whether this error needs a log entry.
    fn failed(&mut self, pin: Pin, error: String, now: Instant) -> bool {
        let previous = self.retry.take().filter(|_| self.pin == pin);
        let changed = previous.as_ref().is_none_or(|retry| retry.error != error);
        // Failed pins retry after 2, 4, 8, 16, 32, then at most 60 seconds.
        let delay = previous.as_ref().map_or(Duration::from_secs(2), |retry| {
            (retry.delay * 2).min(Duration::from_secs(60))
        });
        self.pin = pin;
        self.retry = Some(Retry {
            after: now + delay,
            delay,
            error,
        });
        changed
    }
}

/// Process-owned connector readers. Application stores retain these same readers.
#[derive(Debug)]
pub struct Connectors {
    readers: HashMap<String, Arc<ConnectorReader>>,
    pins: Mutex<HashMap<(String, String), PinState>>,
}

impl Connectors {
    /// Allocate readers for the declared applications before their stores load.
    pub fn new(applications: impl IntoIterator<Item = String>) -> Self {
        Self {
            readers: applications
                .into_iter()
                .map(|name| (name, Arc::new(ConnectorReader::new(HashMap::new()))))
                .collect(),
            pins: Mutex::new(HashMap::new()),
        }
    }

    /// The retained reader of one declared application.
    pub fn reader(&self, application: &str) -> anyhow::Result<Arc<ConnectorReader>> {
        self.readers
            .get(application)
            .cloned()
            .context("connector application is not declared")
    }

    /// Read applied pins with the connection reader's authenticated scope.
    /// A failed pull or compile retains the serving store and backs off per pin.
    /// This method logs new pin errors and database failures for the host poller.
    pub async fn refresh(&self, source: &ConnectorSource) -> anyhow::Result<()> {
        let mut pins = self.pins.lock().await;
        let rows = source
            .postgres
            .released_connection_bindings(
                &source.project,
                &source.env_key,
                &source.manifest_digest,
                wamn_run_state::AuthorityClass::ExecutorPlatform,
            )
            .await?;
        let mut selected = Vec::new();
        for row in rows {
            let package: String = row.get(0);
            let application = wamn_catalog::application_for_package(&source.applications, &package)
                .context("integration binding has no application")?
                .to_owned();
            let alias: String = row.get(1);
            let connection_id: String = row.get(2);
            let version: i32 = row.get(3);
            let name: String = row.get(4);
            let configuration = source
                .credentials
                .version(
                    &source.project,
                    &source.environment,
                    &connection_id,
                    version,
                )
                .await?;
            if configuration.runtime_type != "wasm" {
                continue;
            }
            let pin = Pin {
                digest: configuration
                    .digest
                    .context("integration version has no digest")?,
                connection: name,
                generation: configuration.generation,
            };
            selected.push((
                application,
                alias,
                configuration.integration_type,
                pin,
                configuration.parameters,
            ));
        }
        let mut enabled: HashMap<String, HashSet<String>> = HashMap::new();
        for (application, alias, _, _, _) in &selected {
            enabled
                .entry(application.clone())
                .or_default()
                .insert(alias.clone());
        }
        for (application, reader) in &self.readers {
            reader
                .retain(enabled.get(application).unwrap_or(&HashSet::new()))
                .await;
        }
        pins.retain(|(app, alias), _| enabled.get(app).is_some_and(|names| names.contains(alias)));
        let mut failure = None;
        for (application, alias, integration, pin, parameters) in selected {
            let key = (application.clone(), alias.clone());
            if pins
                .get(&key)
                .is_some_and(|state| !state.ready(&pin, Instant::now()))
            {
                continue;
            }
            // A sibling's failure must not prevent another connector from updating.
            let result = async {
                let reader = self.reader(&application)?;
                let authority = parameters["authority"]
                    .as_str()
                    .context("connector connection needs authority")?;
                let handle = source
                    .credentials
                    .connection_handle(&pin.connection, pin.generation)?;
                let credential = source
                    .credentials
                    .read_connection(&source.project, &handle)
                    .await?;
                let binding = ConnectorBinding::new(authority, credential)?;
                let registry_name =
                    wamn_catalog::parse_component_artifact_base(&source.registry.artifact_base)?;
                let credential = source
                    .registry_credentials
                    .credentials(registry_name.registry())
                    .await?;
                let repository = Repository::with_auth(
                    &source.registry,
                    &pin.digest,
                    RegistryAuth::Basic(
                        credential.username().to_owned(),
                        credential.password().to_owned(),
                    ),
                )?;
                let bytes = wamn_package_artifact::connector::pull_from(
                    &repository,
                    &integration,
                    &pin.digest,
                )
                .await?;
                let prepared = Connector::prepare(bytes, binding).await?;
                reader.replace(&alias, prepared).await
            }
            .await;
            match result {
                Ok(()) => {
                    pins.insert(key, PinState { pin, retry: None });
                }
                Err(error) => {
                    let state = pins.entry(key).or_insert_with(|| PinState {
                        pin: pin.clone(),
                        retry: None,
                    });
                    let message = format!("{error:#}");
                    if state.failed(pin, message.clone(), Instant::now()) {
                        tracing::warn!(
                            application,
                            binding = alias,
                            digest = state.pin.digest,
                            error = message,
                            "connector refresh failed"
                        );
                    }
                    failure =
                        Some(error.context(format!("refresh connector {application}/{alias}")));
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Host-owned inputs. Debug output omits credentials and connection material.
pub struct ConnectorSource {
    pub postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    pub env_key: String,
    pub manifest_digest: String,
    pub applications: wamn_catalog::EnvironmentApplications,
    pub credentials: Arc<ConnectionCredentials>,
    pub registry: PackageRegistry,
    pub registry_credentials: RegistryCredentialSource,
    pub org: String,
    pub project: String,
    pub environment: String,
}

impl std::fmt::Debug for ConnectorSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorSource")
            .field("org", &self.org)
            .field("project", &self.project)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{Pin, PinState};
    use std::time::{Duration, Instant};

    #[test]
    fn failed_pins_back_off_and_log_only_changed_errors() {
        let pin = Pin {
            digest: format!("sha256:{}", "a".repeat(64)),
            connection: "meter".into(),
            generation: 1,
        };
        let mut state = PinState {
            pin: pin.clone(),
            retry: None,
        };
        let mut now = Instant::now();
        assert!(!state.ready(&pin, now), "an applied pin needs no refresh");
        assert!(state.failed(pin.clone(), "pull failed".into(), now));
        for seconds in [2, 4, 8, 16, 32, 60, 60] {
            let due = now + Duration::from_secs(seconds);
            let before = due.checked_sub(Duration::from_millis(1)).unwrap();
            assert!(!state.ready(&pin, before));
            assert!(state.ready(&pin, due));
            now = due;
            assert!(!state.failed(pin.clone(), "pull failed".into(), now));
        }
        now += Duration::from_secs(60);
        assert!(state.failed(pin.clone(), "compile failed".into(), now));
        assert!(!state.ready(&pin, now + Duration::from_secs(59)));
        assert!(state.ready(&pin, now + Duration::from_secs(60)));

        let mut changed = pin.clone();
        changed.digest = format!("sha256:{}", "b".repeat(64));
        assert!(state.ready(&changed, now), "a new digest tries immediately");
        assert!(state.failed(changed.clone(), "compile failed".into(), now));
        assert!(!state.ready(&changed, now + Duration::from_millis(1999)));
        assert!(state.ready(&changed, now + Duration::from_secs(2)));

        changed.generation += 1;
        assert!(
            state.ready(&changed, now),
            "a new generation tries immediately"
        );
        assert!(state.failed(changed.clone(), "compile failed".into(), now));
        assert!(state.ready(&changed, now + Duration::from_secs(2)));
        assert!(!state.ready(&changed, now + Duration::from_millis(1999)));
    }
}

/// Reader for the native HTTP entry. Operation workloads bind their own application.
#[derive(Debug)]
pub struct ConnectorIngress {
    pub postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    pub project: String,
    pub connectors: Arc<Connectors>,
    pub release: Arc<wamn_engine::release_manifest::LoadedRelease>,
}

#[async_trait::async_trait]
impl wash_runtime::plugin::HostPlugin for ConnectorIngress {
    fn id(&self) -> &'static str {
        "wamn-connector-ingress"
    }
    fn world(&self) -> wash_runtime::wit::WitWorld {
        wash_runtime::wit::WitWorld {
            imports: HashSet::from([wash_runtime::wit::WitInterface::from(
                wamn_engine::connector::READER,
            )]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut wash_runtime::engine::workload::WorkloadItem<'a>,
        _: wash_runtime::plugin::WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let manifest = self.release.manifest();
        anyhow::ensure!(
            manifest.composed_entry.is_some(),
            "connector ingress requires a composed entry"
        );
        let package = &manifest
            .composing_member()
            .context("connector entry has no composing member")?
            .package_id;
        let application =
            wamn_catalog::application_for_package(&self.release.scope().applications, package)
                .context("connector entry application is ambiguous")?;
        let mut names = HashSet::new();
        for component in manifest.components.iter().filter(|component| {
            wamn_catalog::application_for_package(
                &self.release.scope().applications,
                &component.package_id,
            ) == Some(application)
        }) {
            names.extend(
                self.postgres
                    .declared_connection_names(
                        &self.project,
                        &self.release.scope().env_key,
                        &component.package_id,
                        &component.descriptor.component_digest,
                    )
                    .await?,
            );
        }
        self.connectors
            .reader(application)?
            .add_to_linker(item, names)
    }
}
