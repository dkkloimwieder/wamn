//! Provision declared event streams and materializer consumers.

mod plan;
pub use plan::validate_inputs;

use std::time::Duration;

use crate::events::{
    advisory_stream_config, consumer_config_matches, materializer_consumer_config,
    source_stream_config, stream_config_matches,
};
use anyhow::{Context as _, ensure};
use async_nats::jetstream::Context;
use async_nats::jetstream::consumer::pull;
use async_nats::jetstream::context::ConsumerInfoErrorKind;
use wamn_registry::Triple;

/// Read the publication-order boundary from stream info without changing broker state.
pub async fn last_published_sequence(
    connection: &crate::nats::PlatformNats,
    target: &Triple,
) -> anyhow::Result<u64> {
    let broker = async_nats::jetstream::new(
        connection
            .connect()
            .await
            .context("connect to the environment event broker")?,
    );
    let name = wamn_event_wire::stream_name(&target.org, &target.project, target.env.as_str());
    let mut stream = broker
        .get_stream(&name)
        .await
        .context("read the environment event stream")?;
    Ok(stream
        .info()
        .await
        .context("read the last published stream sequence")?
        .state
        .last_sequence)
}

/// Discard only the stopped environment's backlog, retaining its stream declarations.
/// The caller excludes writers and waits for CDC publishers before this operation.
pub async fn discard(context: &Context, target: &Triple) -> anyhow::Result<()> {
    let source = wamn_event_wire::stream_name(&target.org, &target.project, &target.env);
    for name in [wamn_event_wire::delivery_advisory_stream(&source), source] {
        let stream = context
            .get_stream(&name)
            .await
            .context("read the environment stream before reset")?;
        stream
            .purge()
            .await
            .with_context(|| format!("discard backlog of {name}"))?;
    }
    Ok(())
}

/// Create missing broker objects with the caller's provisioning credential.
/// Existing objects must match the fields their declaration owns before attachment.
pub async fn provision(
    context: &Context,
    target: &Triple,
    replicas: usize,
    duplicate_window: Duration,
    consumers: &[pull::Config],
) -> anyhow::Result<()> {
    validate_inputs(target, replicas, duplicate_window, consumers)?;
    let source_config = source_stream_config(target, replicas, duplicate_window);
    let advisory_config = advisory_stream_config(target, replicas);
    let source = context
        .get_or_create_stream(source_config.clone())
        .await
        .context("provision the environment source stream")?;
    ensure!(
        stream_config_matches(&source_config, &source.cached_info().config),
        "source stream {} differs from its complete declaration",
        source_config.name
    );
    let advisories = context
        .get_or_create_stream(advisory_config.clone())
        .await
        .context("provision the environment advisory stream")?;
    ensure!(
        stream_config_matches(&advisory_config, &advisories.cached_info().config),
        "advisory stream {} differs from its complete declaration",
        advisory_config.name
    );
    for expected in consumers {
        let durable = expected.durable_name.as_deref().expect("validated durable");
        let actual = match source.consumer_info(durable).await {
            Ok(info) => info.config,
            Err(error) if error.kind() == ConsumerInfoErrorKind::NotFound => source
                .create_consumer_strict(expected.clone())
                .await
                .with_context(|| format!("create declared materializer consumer {durable}"))?
                .cached_info()
                .config
                .clone(),
            Err(error) => return Err(error).context("read declared materializer consumer"),
        };
        ensure!(
            consumer_config_matches(expected, &actual),
            "materializer consumer {durable} differs from its complete declaration"
        );
    }
    Ok(())
}

/// Delete one stream. An absent stream already satisfies deletion.
pub async fn delete(context: &Context, name: &str) -> anyhow::Result<bool> {
    use async_nats::jetstream::{ErrorCode, context::GetStreamErrorKind};
    match context.delete_stream(name).await {
        Ok(_) => Ok(true),
        Err(error) if matches!(error.kind(), GetStreamErrorKind::JetStream(error) if error.error_code() == ErrorCode::STREAM_NOT_FOUND) => {
            Ok(false)
        }
        Err(error) => Err(error).with_context(|| format!("delete stream {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_refuses_foreign_consumers_before_connecting() {
        let target = Triple::new("example", "dev", "fixture");
        let expected = materializer_consumer_config(
            "mat_t_pkg_r1",
            "evt.example.dev.fixture.widget.>",
            Duration::from_secs(30),
            5,
        );
        validate_inputs(
            &target,
            3,
            Duration::from_secs(120),
            std::slice::from_ref(&expected),
        )
        .unwrap();
        for filter in [
            "evt.example.dev.other.widget.>",
            "evt.example.prod.fixture.widget.>",
            "evt.example.fixture.dev",
        ] {
            let config = pull::Config {
                filter_subject: filter.into(),
                ..expected.clone()
            };
            assert!(validate_inputs(&target, 3, Duration::from_secs(120), &[config]).is_err());
        }
        assert!(
            validate_inputs(
                &target,
                3,
                Duration::from_secs(120),
                &[expected.clone(), expected]
            )
            .is_err()
        );
    }

    #[test]
    fn provision_refuses_implicit_or_unbounded_delivery_inputs() {
        let target = Triple::new("example", "dev", "fixture");
        let config = materializer_consumer_config(
            "mat_t_pkg_r1",
            "evt.example.dev.fixture.widget.>",
            Duration::from_secs(30),
            5,
        );
        for replicas in [0, 6] {
            assert!(validate_inputs(&target, replicas, Duration::from_secs(120), &[]).is_err());
        }
        assert!(validate_inputs(&target, 3, Duration::ZERO, &[]).is_err());
        for changed in [
            pull::Config {
                max_deliver: -1,
                ..config.clone()
            },
            pull::Config {
                ack_wait: Duration::ZERO,
                ..config.clone()
            },
            pull::Config {
                max_batch: 0,
                ..config
            },
        ] {
            assert!(validate_inputs(&target, 3, Duration::from_secs(120), &[changed]).is_err());
        }
    }
}
