//! Lazily connect to each project's source stream with its mounted certificate.

use anyhow::Context as _;
use async_nats::jetstream;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OnceCell;
use wamn_catalog::canonical_name::CanonicalName;
use wamn_registry::ProjectEnvKey;

use super::Workload;

type ProjectClient = Arc<OnceCell<Arc<jetstream::Context>>>;

pub(super) struct Broker {
    server: String,
    tls_first: bool,
    projects: Mutex<HashMap<ProjectEnvKey, ProjectClient>>,
}

impl Broker {
    pub(super) fn new(server: String, tls_first: bool) -> Self {
        Self {
            server,
            tls_first,
            projects: Mutex::default(),
        }
    }

    pub(super) async fn consumer(
        &self,
        workload: &Workload,
        workflow_id: &CanonicalName,
    ) -> anyhow::Result<Consumer> {
        let app = &workload.application;
        let key = &workload.project_key;
        let slot = {
            let mut projects = self.projects.lock().expect("workflow broker lock poisoned");
            if let Some(slot) = projects.get(key) {
                Arc::clone(slot)
            } else {
                let slot = Arc::new(OnceCell::new());
                projects.insert(key.clone(), Arc::clone(&slot));
                slot
            }
        };
        let context = slot.get_or_try_init(|| self.connect(key)).await?;
        Ok(Consumer {
            context: Arc::clone(context),
            stream: wamn_event_wire::stream_name(&app.org, &app.project, &app.environment),
            name: wamn_catalog::resource_names::workflow_event_consumer(
                &app.application,
                workflow_id,
            ),
        })
    }

    async fn connect(&self, key: &ProjectEnvKey) -> anyhow::Result<Arc<jetstream::Context>> {
        let directory = std::path::Path::new(super::super::HOST_PLUGIN_SECRET_DIRECTORY)
            .join(wamn_registry::name::nats_certificate(key));
        let inbox = wamn_registry::name::inbox(&wamn_registry::environment_nats_user(key));
        let options = wamn_broker::nats::tls_options(
            async_nats::ConnectOptions::new().custom_inbox_prefix(inbox),
            Some(&directory.join("ca.crt")),
            Some(&directory.join("tls.crt")),
            Some(&directory.join("tls.key")),
            None,
        )?;
        let options = if self.tls_first {
            options.tls_first()
        } else {
            options
        };
        let client = wamn_broker::nats::connect(
            options,
            &self.server,
            wamn_broker::nats::InitialConnect::Once,
        )
        .await
        .with_context(|| format!("connect to the workflow event broker for {key}"))?;
        Ok(Arc::new(jetstream::new(client)))
    }
}

pub(super) struct Consumer {
    context: Arc<jetstream::Context>,
    stream: String,
    name: String,
}

impl Consumer {
    pub(super) async fn last_sequence(&self) -> anyhow::Result<u64> {
        let source = self
            .context
            .get_stream(&self.stream)
            .await
            .with_context(|| format!("read workflow source stream {}", self.stream))?;
        Ok(source.cached_info().state.last_sequence)
    }

    pub(super) async fn exists(&self) -> anyhow::Result<bool> {
        match self
            .context
            .get_consumer_from_stream::<jetstream::consumer::pull::Config, _, _>(
                &self.name,
                &self.stream,
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if consumer_missing(&error) => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("read workflow consumer {}", self.name))
            }
        }
    }

    pub(super) async fn create(&self, filter: &str, start_sequence: u64) -> anyhow::Result<()> {
        let config = jetstream::consumer::pull::Config {
            deliver_policy: jetstream::consumer::DeliverPolicy::ByStartSequence { start_sequence },
            max_deliver: -1,
            max_waiting: 64,
            ..wamn_broker::events::event_consumer_config(
                &self.name,
                filter,
                Duration::from_secs(30),
                5,
            )
        };
        self.context
            .create_consumer_strict_on_stream(config, &self.stream)
            .await
            .with_context(|| format!("create workflow consumer {}", self.name))?;
        Ok(())
    }

    pub(super) async fn delete(&self) -> anyhow::Result<()> {
        match self
            .context
            .delete_consumer_from_stream(&self.name, &self.stream)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if consumer_missing(&error) => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("delete workflow consumer {}", self.name))
            }
        }
    }
}

fn consumer_missing(error: &jetstream::stream::ConsumerError) -> bool {
    use jetstream::{ErrorCode, stream::ConsumerErrorKind};
    matches!(
        error.kind(),
        ConsumerErrorKind::JetStream(error)
            if matches!(error.error_code(), ErrorCode::CONSUMER_NOT_FOUND | ErrorCode::STREAM_NOT_FOUND)
    )
}
