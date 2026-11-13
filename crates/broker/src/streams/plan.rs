use super::{Duration, Triple, ensure, materializer_consumer_config, pull};
use anyhow::Context as _;

pub fn validate_inputs(
    target: &Triple,
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
            target.org.as_str(),
            target.project.as_str(),
            target.env.as_str()
        ]
        .into_iter()
        .all(&token),
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
    let prefix = format!("evt.{}.{}.{}.", target.org, target.project, target.env);
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
