//! Provision declared event streams and materializer consumers.

use std::time::Duration;

use anyhow::{Context as _, ensure};
use async_nats::jetstream::Context;
use async_nats::jetstream::consumer::pull;
use async_nats::jetstream::context::ConsumerInfoErrorKind;
use wamn_control_provision::events::{
    advisory_stream_config, consumer_config_matches, materializer_consumer_config,
    source_stream_config, stream_config_matches,
};
use wamn_control_registry::Triple;

/// Read a private event password and select its native reply prefix.
pub fn connection_options(
    username: &str,
    password_file: &std::path::Path,
) -> anyhow::Result<async_nats::ConnectOptions> {
    ensure!(
        !username.is_empty()
            && username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "event-broker username must be one nonempty broker token"
    );
    let password =
        std::fs::read_to_string(password_file).context("read the event-broker password file")?;
    ensure!(!password.is_empty(), "event-broker password file is empty");
    Ok(async_nats::ConnectOptions::new()
        .user_and_password(username.to_owned(), password)
        .custom_inbox_prefix(format!("_INBOX_{username}")))
}

/// Create missing broker objects with the caller's provisioning credential.
/// Existing objects must match the complete declaration before attachment.
pub async fn provision(
    context: &Context,
    scope: &Triple,
    replicas: usize,
    duplicate_window: Duration,
    consumers: &[pull::Config],
) -> anyhow::Result<()> {
    validate_inputs(scope, replicas, duplicate_window, consumers)?;
    let source_config = source_stream_config(scope, replicas, duplicate_window);
    let advisory_config = advisory_stream_config(scope, replicas);
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

pub(crate) fn validate_inputs(
    scope: &Triple,
    replicas: usize,
    duplicate_window: Duration,
    consumers: &[pull::Config],
) -> anyhow::Result<()> {
    let token = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    ensure!(
        [
            scope.org.as_str(),
            scope.project.as_str(),
            scope.env.as_str()
        ]
        .into_iter()
        .all(|value| token(value)),
        "event coordinates must be nonempty broker tokens"
    );
    ensure!(
        (1..=5).contains(&replicas),
        "event stream replicas must be between one and five"
    );
    ensure!(
        !duplicate_window.is_zero(),
        "event duplicate window must be positive"
    );
    let prefix = format!("evt.{}.{}.{}.", scope.org, scope.project, scope.env);
    let mut names = std::collections::BTreeSet::new();
    for config in consumers {
        let durable = config
            .durable_name
            .as_deref()
            .context("materializer consumer needs a durable name")?;
        ensure!(
            token(durable) && names.insert(durable),
            "materializer durable must be an exact distinct broker name"
        );
        ensure!(
            config
                .filter_subject
                .strip_prefix(&prefix)
                .is_some_and(|tail| !tail.is_empty()),
            "materializer filter must belong to its declared environment"
        );
        let max_deliver = u32::try_from(config.max_deliver)
            .context("materializer max_deliver must be positive")?;
        ensure!(
            max_deliver > 0 && !config.ack_wait.is_zero(),
            "materializer delivery bounds must be positive"
        );
        ensure!(
            config
                == &materializer_consumer_config(
                    durable,
                    &config.filter_subject,
                    config.ack_wait,
                    max_deliver
                ),
            "materializer consumer differs from its bounded declaration"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_refuses_foreign_consumers_before_connecting() {
        let scope = Triple::new("acme", "receiving", "dev");
        let expected = materializer_consumer_config(
            "mat_t_pkg_r1",
            "evt.acme.receiving.dev.receipt.>",
            Duration::from_secs(30),
            5,
        );
        validate_inputs(&scope, 3, Duration::from_secs(120), &[expected.clone()]).unwrap();
        for filter in [
            "evt.acme.wms.dev.receipt.>",
            "evt.acme.receiving.prod.receipt.>",
            "evt.acme.receiving.dev",
        ] {
            let config = pull::Config {
                filter_subject: filter.into(),
                ..expected.clone()
            };
            assert!(validate_inputs(&scope, 3, Duration::from_secs(120), &[config]).is_err());
        }
        assert!(
            validate_inputs(
                &scope,
                3,
                Duration::from_secs(120),
                &[expected.clone(), expected]
            )
            .is_err()
        );
    }

    #[test]
    fn provision_refuses_implicit_or_unbounded_delivery_inputs() {
        let scope = Triple::new("acme", "receiving", "dev");
        let config = materializer_consumer_config(
            "mat_t_pkg_r1",
            "evt.acme.receiving.dev.receipt.>",
            Duration::from_secs(30),
            5,
        );
        for replicas in [0, 6] {
            assert!(validate_inputs(&scope, replicas, Duration::from_secs(120), &[]).is_err());
        }
        assert!(validate_inputs(&scope, 3, Duration::ZERO, &[]).is_err());
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
            assert!(validate_inputs(&scope, 3, Duration::from_secs(120), &[changed]).is_err());
        }
    }
}
